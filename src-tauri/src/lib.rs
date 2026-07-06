use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use hound::{SampleFormat, WavSpec, WavWriter};
use rodio::{Decoder, OutputStream, Sink};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter, LogicalSize, Manager};
use tauri_plugin_dialog::DialogExt;
use tokio::sync::mpsc;
use uuid::Uuid;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptSegment {
    pub id: String,
    pub offset_seconds: f64,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Meeting {
    pub id: String,
    pub title: String,
    pub created_at: String,
    pub updated_at: String,
    pub duration_seconds: f64,
    pub transcript: Vec<TranscriptSegment>,
    pub has_audio: bool,
    pub recording_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptEvent {
    pub meeting_id: String,
    pub segment: TranscriptSegment,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Store {
    meetings: Vec<Meeting>,
    whisper_model_path: Option<String>,
}

impl Default for Store {
    fn default() -> Self {
        Self {
            meetings: Vec::new(),
            whisper_model_path: None,
        }
    }
}

struct RecorderSession {
    meeting_id: String,
    stream: cpal::Stream,
    stop_tx: mpsc::Sender<()>,
    samples: Arc<Mutex<Vec<f32>>>,
    sample_rate: u32,
    started_at: std::time::Instant,
}

// Meetly Light targets Windows only. The active CPAL WASAPI stream is kept behind
// a mutex in Tauri state so commands can stop and drop it from the app thread.
unsafe impl Send for RecorderSession {}

struct PlaybackSession {
    meeting_id: String,
    _stream: OutputStream,
    sink: Sink,
}

unsafe impl Send for PlaybackSession {}

struct AppState {
    store: Mutex<Store>,
    recorder: Mutex<Option<RecorderSession>>,
    playback: Mutex<Option<PlaybackSession>>,
    whisper: Mutex<Option<Arc<WhisperContext>>>,
    data_dir: PathBuf,
}

impl AppState {
    fn new(app: &tauri::App) -> Result<Self> {
        let data_dir = app
            .path()
            .app_data_dir()
            .unwrap_or_else(|_| dirs::data_local_dir().unwrap_or_else(std::env::temp_dir).join("MeetlyLight"));
        fs::create_dir_all(data_dir.join("recordings"))?;
        let store = read_store(&data_dir)?;
        Ok(Self {
            store: Mutex::new(store),
            recorder: Mutex::new(None),
            playback: Mutex::new(None),
            whisper: Mutex::new(None),
            data_dir,
        })
    }

    fn save_store(&self) -> Result<()> {
        let store = self.store.lock().map_err(|_| anyhow!("Store lock poisoned"))?;
        write_store(&self.data_dir, &store)
    }
}

fn store_path(data_dir: &Path) -> PathBuf {
    data_dir.join("store.json")
}

fn read_store(data_dir: &Path) -> Result<Store> {
    let path = store_path(data_dir);
    if !path.exists() {
        let store = Store::default();
        write_store(data_dir, &store)?;
        return Ok(store);
    }
    let raw = fs::read_to_string(path)?;
    Ok(serde_json::from_str(&raw).unwrap_or_default())
}

fn write_store(data_dir: &Path, store: &Store) -> Result<()> {
    fs::create_dir_all(data_dir)?;
    fs::write(store_path(data_dir), serde_json::to_string_pretty(store)?)?;
    Ok(())
}

fn now_string() -> String {
    Utc::now().to_rfc3339()
}

fn format_timestamp(seconds: f64) -> String {
    let total = seconds.max(0.0).floor() as u64;
    format!("{:02}:{:02}", total / 60, total % 60)
}

fn make_meeting(title: Option<String>) -> Meeting {
    let now = now_string();
    Meeting {
        id: format!("meeting-{}", Uuid::new_v4()),
        title: title.unwrap_or_else(|| format!("Meeting {}", chrono::Local::now().format("%Y-%m-%d %H-%M"))),
        created_at: now.clone(),
        updated_at: now,
        duration_seconds: 0.0,
        transcript: Vec::new(),
        has_audio: false,
        recording_path: None,
    }
}

fn resample_to_16khz(samples: &[f32], source_rate: u32) -> Vec<f32> {
    if source_rate == 16_000 {
        return samples.to_vec();
    }
    let ratio = source_rate as f64 / 16_000.0;
    let out_len = (samples.len() as f64 / ratio).floor() as usize;
    (0..out_len)
        .map(|idx| {
            let source_idx = (idx as f64 * ratio).floor() as usize;
            samples.get(source_idx).copied().unwrap_or_default()
        })
        .collect()
}

fn transcribe_samples(context: Arc<WhisperContext>, samples_16k: Vec<f32>) -> Result<String> {
    if samples_16k.len() < 16_000 {
        return Ok(String::new());
    }
    let mut state = context.create_state().context("Failed to create Whisper state")?;
    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_print_progress(false);
    params.set_print_special(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    params.set_language(Some("en"));
    state.full(params, &samples_16k).context("Whisper transcription failed")?;
    let segments = state.full_n_segments();
    let mut text = String::new();
    for index in 0..segments {
        let Some(segment) = state.get_segment(index) else {
            continue;
        };
        let segment_text = segment.to_str_lossy()?;
        text.push_str(segment_text.trim());
        text.push(' ');
    }
    Ok(text.trim().to_string())
}

fn write_wav(path: &Path, samples: &[f32], sample_rate: u32) -> Result<()> {
    let spec = WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 16,
        sample_format: SampleFormat::Int,
    };
    let mut writer = WavWriter::create(path, spec)?;
    for sample in samples {
        let value = (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
        writer.write_sample(value)?;
    }
    writer.finalize()?;
    Ok(())
}

fn safe_file_stem(value: &str) -> String {
    let sanitized: String = value
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' { ch } else { '-' })
        .collect();
    sanitized.trim_matches('-').to_lowercase()
}

fn find_meeting(store: &Store, meeting_id: &str) -> Result<Meeting> {
    store
        .meetings
        .iter()
        .find(|item| item.id == meeting_id)
        .cloned()
        .ok_or_else(|| anyhow!("Meeting not found"))
}

fn open_path_in_explorer(path: &Path) -> Result<()> {
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("explorer")
            .arg(path)
            .spawn()
            .context("Failed to open File Explorer")?;
    }
    Ok(())
}

fn select_path_in_explorer(path: &Path) -> Result<()> {
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("explorer")
            .arg(format!("/select,{}", path.to_string_lossy()))
            .spawn()
            .context("Failed to open File Explorer")?;
    }
    Ok(())
}

#[tauri::command]
fn get_meetings(state: tauri::State<'_, AppState>) -> Result<Vec<Meeting>, String> {
    let store = state.store.lock().map_err(|_| "Store lock poisoned".to_string())?;
    Ok(store.meetings.clone())
}

#[tauri::command]
fn load_whisper_model(state: tauri::State<'_, AppState>, model_path: String) -> Result<(), String> {
    let model_path = model_path.trim().to_string();
    if model_path.is_empty() {
        return Err("Enter the full path to a local Whisper .bin model file.".to_string());
    }

    let path = PathBuf::from(&model_path);
    if !path.exists() {
        return Err(format!("Whisper model file not found: {}", model_path));
    }
    if !path.is_file() {
        return Err(format!("Whisper model path is not a file: {}", model_path));
    }
    if path.extension().and_then(|extension| extension.to_str()) != Some("bin") {
        return Err("Whisper model must be a .bin file, for example ggml-base.en.bin.".to_string());
    }

    let context = WhisperContext::new_with_params(&model_path, WhisperContextParameters::default())
        .map_err(|error| format!("Failed to load Whisper model: {error}"))?;
    *state.whisper.lock().map_err(|_| "Whisper lock poisoned".to_string())? = Some(Arc::new(context));
    {
        let mut store = state.store.lock().map_err(|_| "Store lock poisoned".to_string())?;
        store.whisper_model_path = Some(model_path);
    }
    state.save_store().map_err(|error| error.to_string())
}

#[tauri::command]
fn get_whisper_model_status(state: tauri::State<'_, AppState>) -> Result<String, String> {
    let loaded = state.whisper.lock().map_err(|_| "Whisper lock poisoned".to_string())?.is_some();
    if loaded {
        return Ok("loaded".to_string());
    }
    let store = state.store.lock().map_err(|_| "Store lock poisoned".to_string())?;
    Ok(store.whisper_model_path.clone().unwrap_or_else(|| "missing".to_string()))
}

#[tauri::command]
fn select_whisper_model(app: AppHandle) -> Result<Option<String>, String> {
    let selected = app
        .dialog()
        .file()
        .add_filter("Whisper ggml model", &["bin"])
        .blocking_pick_file();

    Ok(selected.map(|path| path.to_string()))
}

#[tauri::command]
async fn start_recording(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    meeting_title: Option<String>,
) -> Result<Meeting, String> {
    if state.recorder.lock().map_err(|_| "Recorder lock poisoned".to_string())?.is_some() {
        return Err("Recording already in progress".to_string());
    }

    let context = state
        .whisper
        .lock()
        .map_err(|_| "Whisper lock poisoned".to_string())?
        .clone()
        .ok_or_else(|| "Load a local Whisper model before recording.".to_string())?;

    let meeting = make_meeting(meeting_title);
    {
        let mut store = state.store.lock().map_err(|_| "Store lock poisoned".to_string())?;
        store.meetings.insert(0, meeting.clone());
    }
    state.save_store().map_err(|error| error.to_string())?;

    let host = cpal::default_host();
    let device = host.default_input_device().ok_or_else(|| "No default microphone found".to_string())?;
    let config = device.default_input_config().map_err(|error| error.to_string())?;
    let sample_rate = config.sample_rate().0;
    let samples = Arc::new(Mutex::new(Vec::<f32>::new()));
    let pending = Arc::new(Mutex::new(Vec::<f32>::new()));
    let samples_for_callback = samples.clone();
    let pending_for_callback = pending.clone();
    let error_callback = |error| eprintln!("Audio stream error: {error}");

    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => device.build_input_stream(
            &config.into(),
            move |data: &[f32], _| {
                samples_for_callback.lock().map(|mut buffer| buffer.extend_from_slice(data)).ok();
                pending_for_callback.lock().map(|mut buffer| buffer.extend_from_slice(data)).ok();
            },
            error_callback,
            None,
        ),
        cpal::SampleFormat::I16 => device.build_input_stream(
            &config.into(),
            move |data: &[i16], _| {
                let converted: Vec<f32> = data.iter().map(|sample| *sample as f32 / i16::MAX as f32).collect();
                samples_for_callback.lock().map(|mut buffer| buffer.extend_from_slice(&converted)).ok();
                pending_for_callback.lock().map(|mut buffer| buffer.extend_from_slice(&converted)).ok();
            },
            error_callback,
            None,
        ),
        cpal::SampleFormat::U16 => device.build_input_stream(
            &config.into(),
            move |data: &[u16], _| {
                let converted: Vec<f32> = data.iter().map(|sample| (*sample as f32 / u16::MAX as f32) * 2.0 - 1.0).collect();
                samples_for_callback.lock().map(|mut buffer| buffer.extend_from_slice(&converted)).ok();
                pending_for_callback.lock().map(|mut buffer| buffer.extend_from_slice(&converted)).ok();
            },
            error_callback,
            None,
        ),
        _ => return Err("Unsupported microphone sample format".to_string()),
    }
    .map_err(|error| error.to_string())?;

    stream.play().map_err(|error| error.to_string())?;

    let (stop_tx, mut stop_rx) = mpsc::channel::<()>(1);
    let meeting_id = meeting.id.clone();
    let data_dir = state.data_dir.clone();
    let app_for_task = app.clone();
    let pending_for_task = pending.clone();

    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        let mut offset_seconds = 0.0;
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    let chunk = pending_for_task.lock().map(|mut buffer| std::mem::take(&mut *buffer)).unwrap_or_default();
                    if chunk.is_empty() {
                        continue;
                    }
                    let samples_16k = resample_to_16khz(&chunk, sample_rate);
                    match transcribe_samples(context.clone(), samples_16k) {
                        Ok(text) if !text.is_empty() => {
                            let segment = TranscriptSegment {
                                id: format!("segment-{}", Uuid::new_v4()),
                                offset_seconds,
                                text,
                            };
                            offset_seconds += 5.0;
                            let _ = append_segment_to_store(&data_dir, &meeting_id, segment.clone());
                            let _ = app_for_task.emit("transcript-segment", TranscriptEvent {
                                meeting_id: meeting_id.clone(),
                                segment,
                            });
                        }
                        Ok(_) => {}
                        Err(error) => {
                            let _ = app_for_task.emit("transcription-error", error.to_string());
                        }
                    }
                }
                _ = stop_rx.recv() => break,
            }
        }
    });

    *state.recorder.lock().map_err(|_| "Recorder lock poisoned".to_string())? = Some(RecorderSession {
        meeting_id: meeting.id.clone(),
        stream,
        stop_tx,
        samples,
        sample_rate,
        started_at: std::time::Instant::now(),
    });

    Ok(meeting)
}

fn append_segment_to_store(data_dir: &Path, meeting_id: &str, segment: TranscriptSegment) -> Result<()> {
    let mut store = read_store(data_dir)?;
    if let Some(meeting) = store.meetings.iter_mut().find(|item| item.id == meeting_id) {
        meeting.transcript.push(segment);
        meeting.updated_at = now_string();
    }
    write_store(data_dir, &store)
}

#[tauri::command]
async fn stop_recording(state: tauri::State<'_, AppState>) -> Result<Meeting, String> {
    let session = state
        .recorder
        .lock()
        .map_err(|_| "Recorder lock poisoned".to_string())?
        .take()
        .ok_or_else(|| "Recording is not running".to_string())?;

    let _ = session.stop_tx.send(()).await;
    drop(session.stream);

    let samples = session.samples.lock().map_err(|_| "Sample buffer lock poisoned".to_string())?.clone();
    let samples_16k = resample_to_16khz(&samples, session.sample_rate);
    let duration = session.started_at.elapsed().as_secs_f64();
    let recording_path = state.data_dir.join("recordings").join(format!("{}.wav", session.meeting_id));
    write_wav(&recording_path, &samples_16k, 16_000).map_err(|error| error.to_string())?;

    let updated = {
        let mut store = state.store.lock().map_err(|_| "Store lock poisoned".to_string())?;
        let meeting = store
            .meetings
            .iter_mut()
            .find(|item| item.id == session.meeting_id)
            .ok_or_else(|| "Meeting not found".to_string())?;
        meeting.duration_seconds = duration;
        meeting.has_audio = true;
        meeting.recording_path = Some(recording_path.to_string_lossy().to_string());
        meeting.updated_at = now_string();
        meeting.clone()
    };
    state.save_store().map_err(|error| error.to_string())?;
    Ok(updated)
}

#[tauri::command]
fn rename_meeting(state: tauri::State<'_, AppState>, meeting_id: String, title: String) -> Result<Meeting, String> {
    let updated = {
        let mut store = state.store.lock().map_err(|_| "Store lock poisoned".to_string())?;
        let meeting = store
            .meetings
            .iter_mut()
            .find(|item| item.id == meeting_id)
            .ok_or_else(|| "Meeting not found".to_string())?;
        meeting.title = title;
        meeting.updated_at = now_string();
        meeting.clone()
    };
    state.save_store().map_err(|error| error.to_string())?;
    Ok(updated)
}

#[tauri::command]
fn delete_meeting(state: tauri::State<'_, AppState>, meeting_id: String) -> Result<(), String> {
    let recording_path = {
        let mut store = state.store.lock().map_err(|_| "Store lock poisoned".to_string())?;
        let recording_path = store
            .meetings
            .iter()
            .find(|item| item.id == meeting_id)
            .and_then(|meeting| meeting.recording_path.clone());
        store.meetings.retain(|meeting| meeting.id != meeting_id);
        recording_path
    };
    if let Some(path) = recording_path {
        let _ = fs::remove_file(path);
    }
    {
        let mut playback = state.playback.lock().map_err(|_| "Playback lock poisoned".to_string())?;
        if playback.as_ref().map(|session| session.meeting_id == meeting_id).unwrap_or(false) {
            if let Some(session) = playback.take() {
                session.sink.stop();
            }
        }
    }
    state.save_store().map_err(|error| error.to_string())
}

#[tauri::command]
fn export_transcript(app: AppHandle, state: tauri::State<'_, AppState>, meeting_id: String) -> Result<Option<String>, String> {
    let store = state.store.lock().map_err(|_| "Store lock poisoned".to_string())?;
    let meeting = find_meeting(&store, &meeting_id).map_err(|error| error.to_string())?;
    let mut text = format!(
        "Meeting: {}\nCreated: {}\nDuration: {}\n\n",
        meeting.title,
        meeting.created_at,
        format_timestamp(meeting.duration_seconds)
    );
    for segment in &meeting.transcript {
        text.push_str(&format!("[{}] {}\n", format_timestamp(segment.offset_seconds), segment.text));
    }
    drop(store);

    let default_name = format!("{}-transcript.txt", safe_file_stem(&meeting.title));
    let Some(path) = app
        .dialog()
        .file()
        .add_filter("Transcript text", &["txt"])
        .set_file_name(&default_name)
        .blocking_save_file()
    else {
        return Ok(None);
    };

    let path = PathBuf::from(path.to_string());
    fs::write(&path, text).map_err(|error| error.to_string())?;
    Ok(Some(path.to_string_lossy().to_string()))
}

#[tauri::command]
fn export_audio(app: AppHandle, state: tauri::State<'_, AppState>, meeting_id: String) -> Result<Option<String>, String> {
    let store = state.store.lock().map_err(|_| "Store lock poisoned".to_string())?;
    let meeting = find_meeting(&store, &meeting_id).map_err(|error| error.to_string())?;
    let source = meeting
        .recording_path
        .clone()
        .ok_or_else(|| "Recording not found".to_string())?;
    drop(store);

    let default_name = format!("{}-recording.wav", safe_file_stem(&meeting.title));
    let Some(path) = app
        .dialog()
        .file()
        .add_filter("WAV audio", &["wav"])
        .set_file_name(&default_name)
        .blocking_save_file()
    else {
        return Ok(None);
    };

    let path = PathBuf::from(path.to_string());
    fs::copy(&source, &path).map_err(|error| error.to_string())?;
    Ok(Some(path.to_string_lossy().to_string()))
}

#[tauri::command]
fn open_meeting_folder(state: tauri::State<'_, AppState>, meeting_id: String) -> Result<(), String> {
    let store = state.store.lock().map_err(|_| "Store lock poisoned".to_string())?;
    let meeting = find_meeting(&store, &meeting_id).map_err(|error| error.to_string())?;
    let Some(recording_path) = meeting.recording_path else {
        return open_path_in_explorer(&state.data_dir).map_err(|error| error.to_string());
    };
    let path = PathBuf::from(recording_path);
    if path.exists() {
        select_path_in_explorer(&path).map_err(|error| error.to_string())
    } else if let Some(parent) = path.parent() {
        open_path_in_explorer(parent).map_err(|error| error.to_string())
    } else {
        open_path_in_explorer(&state.data_dir).map_err(|error| error.to_string())
    }
}

#[tauri::command]
fn play_recording(state: tauri::State<'_, AppState>, meeting_id: String, offset_seconds: Option<f64>) -> Result<(), String> {
    let store = state.store.lock().map_err(|_| "Store lock poisoned".to_string())?;
    let meeting = find_meeting(&store, &meeting_id).map_err(|error| error.to_string())?;
    let recording_path = meeting
        .recording_path
        .ok_or_else(|| "Recording not found".to_string())?;
    drop(store);

    let file = fs::File::open(&recording_path).map_err(|error| error.to_string())?;
    let source = Decoder::new(BufReader::new(file)).map_err(|error| error.to_string())?;
    let (stream, handle) = OutputStream::try_default().map_err(|error| error.to_string())?;
    let sink = Sink::try_new(&handle).map_err(|error| error.to_string())?;
    sink.append(source);
    if let Some(offset) = offset_seconds {
        if offset > 0.0 {
            let _ = sink.try_seek(Duration::from_secs_f64(offset));
        }
    }
    sink.play();

    let mut playback = state.playback.lock().map_err(|_| "Playback lock poisoned".to_string())?;
    if let Some(existing) = playback.take() {
        existing.sink.stop();
    }
    *playback = Some(PlaybackSession {
        meeting_id,
        _stream: stream,
        sink,
    });
    Ok(())
}

#[tauri::command]
fn pause_playback(state: tauri::State<'_, AppState>) -> Result<(), String> {
    let playback = state.playback.lock().map_err(|_| "Playback lock poisoned".to_string())?;
    let Some(playback) = playback.as_ref() else {
        return Err("No active playback".to_string());
    };
    playback.sink.pause();
    Ok(())
}

#[tauri::command]
fn resume_playback(state: tauri::State<'_, AppState>) -> Result<(), String> {
    let playback = state.playback.lock().map_err(|_| "Playback lock poisoned".to_string())?;
    let Some(playback) = playback.as_ref() else {
        return Err("No active playback".to_string());
    };
    playback.sink.play();
    Ok(())
}

#[tauri::command]
fn set_mini_mode(app: AppHandle, enabled: bool) -> Result<(), String> {
    let Some(window) = app.get_webview_window("main") else {
        return Err("Main window not found".to_string());
    };
    let size = if enabled {
        LogicalSize::new(360.0, 200.0)
    } else {
        LogicalSize::new(420.0, 760.0)
    };
    window.set_size(size).map_err(|error| error.to_string())?;
    window.set_always_on_top(enabled).map_err(|error| error.to_string())?;
    Ok(())
}

pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let state = AppState::new(app).map_err(|error| Box::<dyn std::error::Error>::from(error.to_string()))?;
            app.manage(state);
            Ok(())
        })
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            get_meetings,
            load_whisper_model,
            select_whisper_model,
            get_whisper_model_status,
            start_recording,
            stop_recording,
            rename_meeting,
            delete_meeting,
            export_transcript,
            export_audio,
            open_meeting_folder,
            play_recording,
            pause_playback,
            resume_playback,
            set_mini_mode,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Meetly Light");
}
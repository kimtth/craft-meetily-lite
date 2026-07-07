use crate::audio::{
    build_capture_streams, list_input_devices, list_output_devices, mix_sources, pcm16_base64,
    prepare_playback, stop_playback_session, write_wav, CaptureBuffers,
};
use crate::azure_auth::{self, AzureCliAccessToken, AzureCliStatus};
use crate::{
    default_capture_mode, default_language, fallback_model_dirs, read_store, write_store,
    AppSettings, AppState, AudioChunkEvent, AudioInputDevice, AudioOutputDevice, Meeting,
    PlaybackSession, RecorderSession, Store, TranscriptEvent, TranscriptSegment,
    TranscriptionEngine, WhisperModelStatus,
};
use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter, LogicalSize, Manager};
use tauri_plugin_dialog::DialogExt;
use tokio::sync::mpsc;
use uuid::Uuid;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

const TRANSCRIPT_CHUNK_SECONDS: u64 = 5;
const TRANSCRIPT_SAMPLE_RATE: f64 = 16_000.0;

fn now_string() -> String {
    Utc::now().to_rfc3339()
}

/// Number of CPU threads Whisper should use for inference. Using the machine's
/// available parallelism keeps transcription faster than real time so the live
/// loop does not fall behind. Falls back to 4 if the count cannot be queried.
fn whisper_thread_count() -> std::os::raw::c_int {
    std::thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(4)
        .clamp(1, 16) as std::os::raw::c_int
}

fn format_timestamp(seconds: f64) -> String {
    let total = seconds.max(0.0).floor() as u64;
    format!("{:02}:{:02}", total / 60, total % 60)
}

fn make_meeting(
    title: Option<String>,
    audio_device_id: Option<String>,
    audio_device_name: Option<String>,
    system_audio_device_id: Option<String>,
    system_audio_device_name: Option<String>,
    capture_mode: String,
    language: String,
) -> Meeting {
    let now = now_string();
    Meeting {
        id: format!("meeting-{}", Uuid::new_v4()),
        title: title.unwrap_or_else(|| {
            format!("Meeting {}", chrono::Local::now().format("%Y-%m-%d %H-%M"))
        }),
        created_at: now.clone(),
        updated_at: now,
        duration_seconds: 0.0,
        transcript: Vec::new(),
        has_audio: false,
        recording_path: None,
        audio_device_id,
        audio_device_name,
        system_audio_device_id,
        system_audio_device_name,
        capture_mode,
        language,
    }
}

fn transcribe_samples(
    context: Arc<WhisperContext>,
    samples_16k: Vec<f32>,
    language: &str,
) -> Result<String> {
    if samples_16k.len() < 16_000 {
        return Ok(String::new());
    }
    let mut state = context
        .create_state()
        .context("Failed to create Whisper state")?;
    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_n_threads(whisper_thread_count());
    // Each chunk is transcribed independently, so previous chunk tokens must not
    // be reused as a prompt (that both slows inference and causes cross-chunk
    // repetition/hallucination).
    params.set_no_context(true);
    params.set_print_progress(false);
    params.set_print_special(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    if language == "auto" {
        params.set_language(None);
    } else {
        params.set_language(Some(language));
    }
    state
        .full(params, &samples_16k)
        .context("Whisper transcription failed")?;
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

fn normalize_transcription_engine(value: Option<String>) -> TranscriptionEngine {
    match value
        .as_deref()
        .map(str::trim)
        .map(str::to_lowercase)
        .as_deref()
    {
        Some("azure") => TranscriptionEngine::Azure,
        _ => TranscriptionEngine::Local,
    }
}

fn safe_file_stem(value: &str) -> String {
    let sanitized: String = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
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

fn normalize_language(language: Option<String>) -> String {
    let trimmed = language
        .unwrap_or_else(default_language)
        .trim()
        .to_lowercase();
    if trimmed.is_empty() {
        default_language()
    } else {
        trimmed
    }
}

fn normalize_capture_mode(capture_mode: Option<String>) -> String {
    match capture_mode.unwrap_or_else(default_capture_mode).as_str() {
        "microphone" => "microphone".to_string(),
        "system" => "system".to_string(),
        _ => default_capture_mode(),
    }
}

pub(crate) fn append_segment_to_store(
    data_dir: &Path,
    meeting_id: &str,
    segment: TranscriptSegment,
) -> Result<()> {
    let mut store = read_store(data_dir)?;
    if let Some(meeting) = store.meetings.iter_mut().find(|item| item.id == meeting_id) {
        meeting.transcript.push(segment);
        meeting.updated_at = now_string();
    }
    write_store(data_dir, &store)
}

#[tauri::command]
pub(crate) fn get_meetings(state: tauri::State<'_, AppState>) -> Result<Vec<Meeting>, String> {
    let store = state
        .store
        .lock()
        .map_err(|_| "Store lock poisoned".to_string())?;
    Ok(store.meetings.clone())
}

#[tauri::command]
pub(crate) fn get_settings(state: tauri::State<'_, AppState>) -> Result<AppSettings, String> {
    let store = state
        .store
        .lock()
        .map_err(|_| "Store lock poisoned".to_string())?;
    Ok(store.settings.clone())
}

#[tauri::command]
pub(crate) fn save_settings(
    state: tauri::State<'_, AppState>,
    settings: AppSettings,
) -> Result<(), String> {
    {
        let mut store = state
            .store
            .lock()
            .map_err(|_| "Store lock poisoned".to_string())?;
        store.settings = settings;
    }
    state.save_store().map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn load_whisper_model(
    state: tauri::State<'_, AppState>,
    model_path: String,
) -> Result<(), String> {
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
    *state
        .whisper
        .lock()
        .map_err(|_| "Whisper lock poisoned".to_string())? = Some(Arc::new(context));
    {
        let mut store = state
            .store
            .lock()
            .map_err(|_| "Store lock poisoned".to_string())?;
        store.whisper_model_path = Some(model_path);
    }
    state.save_store().map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn get_whisper_model_status(
    state: tauri::State<'_, AppState>,
) -> Result<WhisperModelStatus, String> {
    let loaded = state
        .whisper
        .lock()
        .map_err(|_| "Whisper lock poisoned".to_string())?
        .is_some();
    let store = state
        .store
        .lock()
        .map_err(|_| "Store lock poisoned".to_string())?;
    Ok(WhisperModelStatus {
        loaded,
        path: store.whisper_model_path.clone(),
    })
}

fn get_or_load_whisper_context(state: &AppState) -> Result<Arc<WhisperContext>, String> {
    if let Some(context) = state
        .whisper
        .lock()
        .map_err(|_| "Whisper lock poisoned".to_string())?
        .clone()
    {
        return Ok(context);
    }

    let model_path = {
        let store = state
            .store
            .lock()
            .map_err(|_| "Store lock poisoned".to_string())?;
        store.whisper_model_path.clone()
    };

    // Fall back to auto-detecting a local model so recording can start without a
    // manual "Load Model" step. Persist the detected path for later sessions.
    let model_path = match model_path {
        Some(path) if Path::new(&path).is_file() => path,
        _ => {
            let detected = crate::find_default_model(&fallback_model_dirs(&state.data_dir))
                .ok_or_else(|| "No local Whisper model found. Add a ggml .bin model in the app models folder or load one from Settings.".to_string())?;
            let detected = detected.to_string_lossy().to_string();
            {
                let mut store = state
                    .store
                    .lock()
                    .map_err(|_| "Store lock poisoned".to_string())?;
                store.whisper_model_path = Some(detected.clone());
            }
            let _ = state.save_store();
            detected
        }
    };

    let context = WhisperContext::new_with_params(&model_path, WhisperContextParameters::default())
        .map_err(|error| format!("Failed to load Whisper model: {error}"))?;
    let context = Arc::new(context);
    *state
        .whisper
        .lock()
        .map_err(|_| "Whisper lock poisoned".to_string())? = Some(context.clone());
    Ok(context)
}

#[tauri::command]
pub(crate) fn select_whisper_model(app: AppHandle) -> Result<Option<String>, String> {
    let selected = app
        .dialog()
        .file()
        .add_filter("Whisper ggml model", &["bin"])
        .blocking_pick_file();

    Ok(selected.map(|path| path.to_string()))
}

#[tauri::command]
pub(crate) fn list_audio_input_devices() -> Result<Vec<AudioInputDevice>, String> {
    list_input_devices()
}

#[tauri::command]
pub(crate) fn list_audio_output_devices() -> Result<Vec<AudioOutputDevice>, String> {
    list_output_devices()
}

#[tauri::command]
pub(crate) fn start_recording(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    meeting_title: Option<String>,
    audio_device_id: Option<String>,
    system_audio_device_id: Option<String>,
    capture_mode: Option<String>,
    language: Option<String>,
    transcription_engine: Option<String>,
) -> Result<Meeting, String> {
    if state
        .recorder
        .lock()
        .map_err(|_| "Recorder lock poisoned".to_string())?
        .is_some()
    {
        return Err("Recording already in progress".to_string());
    }

    let transcription_engine = normalize_transcription_engine(transcription_engine);
    let context = if transcription_engine == TranscriptionEngine::Local {
        Some(get_or_load_whisper_context(&state)?)
    } else {
        None
    };

    let language = normalize_language(language);
    let capture_mode = normalize_capture_mode(capture_mode);
    let capture_microphone = capture_mode != "system";
    let capture_system = capture_mode != "microphone";
    let samples = Arc::new(Mutex::new(CaptureBuffers::default()));
    let pending = Arc::new(Mutex::new(CaptureBuffers::default()));

    // `cpal::Stream` is `!Send`, so a dedicated thread owns the streams for the
    // whole recording. `start_recording` only keeps `Send` control handles,
    // which makes "a stream crossing threads" unrepresentable and removes the
    // need for any `unsafe impl Send`.
    let (setup_tx, setup_rx) =
        std::sync::mpsc::channel::<Result<crate::audio::AudioSetup, String>>();
    let (audio_stop_tx, audio_stop_rx) = std::sync::mpsc::channel::<()>();
    let samples_for_audio = samples.clone();
    let pending_for_audio = pending.clone();
    let audio_thread = std::thread::spawn(move || {
        match build_capture_streams(
            capture_microphone,
            capture_system,
            audio_device_id.as_deref(),
            system_audio_device_id.as_deref(),
            &capture_mode,
            samples_for_audio,
            pending_for_audio,
        ) {
            Ok((streams, setup)) => {
                if setup_tx.send(Ok(setup)).is_err() {
                    return;
                }
                // Keep the streams alive on this thread until stop is signalled,
                // then drop them here so CPAL tears down on its owning thread.
                let _ = audio_stop_rx.recv();
                drop(streams);
            }
            Err(error) => {
                let _ = setup_tx.send(Err(error));
            }
        }
    });

    let setup = match setup_rx.recv() {
        Ok(Ok(setup)) => setup,
        Ok(Err(error)) => return Err(error),
        Err(_) => return Err("Audio capture thread stopped unexpectedly".to_string()),
    };

    let meeting = make_meeting(
        meeting_title,
        setup.resolved_audio_device_id,
        setup.audio_device_name,
        setup.resolved_system_audio_device_id,
        setup.system_audio_device_name,
        setup.resolved_capture_mode,
        language.clone(),
    );

    let persisted = state
        .store
        .lock()
        .map_err(|_| "Store lock poisoned".to_string())
        .map(|mut store| store.meetings.insert(0, meeting.clone()))
        .and_then(|_| state.save_store().map_err(|error| error.to_string()));
    if let Err(error) = persisted {
        let _ = audio_stop_tx.send(());
        let _ = audio_thread.join();
        return Err(error);
    }

    let (stop_tx, mut stop_rx) = mpsc::channel::<()>(1);
    let meeting_id = meeting.id.clone();
    let data_dir = state.data_dir.clone();
    let app_for_task = app.clone();
    let pending_for_task = pending.clone();
    let language_state = Arc::new(Mutex::new(language.clone()));
    let language_for_task = language_state.clone();

    tauri::async_runtime::spawn(async move {
        let interval_duration = if transcription_engine == TranscriptionEngine::Azure {
            Duration::from_millis(250)
        } else {
            Duration::from_secs(TRANSCRIPT_CHUNK_SECONDS)
        };
        let mut interval = tokio::time::interval(interval_duration);
        let mut offset_seconds = 0.0;
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    let chunk = pending_for_task
                        .lock()
                        .map(|mut buffer| CaptureBuffers {
                            microphone: std::mem::take(&mut buffer.microphone),
                            system: std::mem::take(&mut buffer.system),
                        })
                        .unwrap_or_default();
                    let samples_16k = mix_sources(&chunk.microphone, &chunk.system);
                    if samples_16k.is_empty() {
                        continue;
                    }
                    let segment_offset = offset_seconds;
                    offset_seconds += samples_16k.len() as f64 / TRANSCRIPT_SAMPLE_RATE;

                    if transcription_engine == TranscriptionEngine::Azure {
                        let _ = app_for_task.emit("audio-chunk", AudioChunkEvent {
                            meeting_id: meeting_id.clone(),
                            offset_seconds: segment_offset,
                            pcm_base64: pcm16_base64(&samples_16k),
                        });
                        continue;
                    }

                    let chunk_language = language_for_task.lock().map(|value| value.clone()).unwrap_or_else(|_| default_language());
                    let Some(context_for_chunk) = context.clone() else {
                        continue;
                    };
                    // Whisper inference is CPU-bound and blocking; run it on the
                    // blocking thread pool so it never stalls the async runtime
                    // (event emission, stop signal, other tasks).
                    let result = tokio::task::spawn_blocking(move || {
                        transcribe_samples(context_for_chunk, samples_16k, &chunk_language)
                    })
                    .await
                    .unwrap_or_else(|error| Err(anyhow!("Transcription task panicked: {error}")));
                    match result {
                        Ok(text) if !text.is_empty() => {
                            let segment = TranscriptSegment {
                                id: format!("segment-{}", Uuid::new_v4()),
                                offset_seconds: segment_offset,
                                text,
                            };
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

    *state
        .recorder
        .lock()
        .map_err(|_| "Recorder lock poisoned".to_string())? = Some(RecorderSession {
        meeting_id: meeting.id.clone(),
        stop_tx,
        audio_stop_tx,
        audio_thread,
        samples,
        language: language_state,
        started_at: std::time::Instant::now(),
    });

    Ok(meeting)
}

#[tauri::command]
pub(crate) fn add_transcript_segment(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    meeting_id: String,
    text: String,
    offset_seconds: f64,
) -> Result<TranscriptSegment, String> {
    let text = text.trim().to_string();
    if text.is_empty() {
        return Err("Transcript segment text is empty".to_string());
    }

    let segment = TranscriptSegment {
        id: format!("segment-{}", Uuid::new_v4()),
        offset_seconds: offset_seconds.max(0.0),
        text,
    };
    {
        let mut store = state
            .store
            .lock()
            .map_err(|_| "Store lock poisoned".to_string())?;
        let meeting = store
            .meetings
            .iter_mut()
            .find(|item| item.id == meeting_id)
            .ok_or_else(|| "Meeting not found".to_string())?;
        meeting.transcript.push(segment.clone());
        meeting.updated_at = now_string();
    }
    state.save_store().map_err(|error| error.to_string())?;
    let _ = app.emit(
        "transcript-segment",
        TranscriptEvent {
            meeting_id,
            segment: segment.clone(),
        },
    );
    Ok(segment)
}

#[tauri::command]
pub(crate) async fn get_azure_cli_access_token(
    tenant_id: Option<String>,
    subscription_id: Option<String>,
) -> Result<AzureCliAccessToken, String> {
    azure_auth::get_azure_cli_access_token(tenant_id, subscription_id).await
}

#[tauri::command]
pub(crate) async fn sign_in_azure_cli(
    tenant_id: Option<String>,
    subscription_id: Option<String>,
) -> AzureCliStatus {
    azure_auth::sign_in_azure_cli(tenant_id, subscription_id).await
}

#[tauri::command]
pub(crate) async fn check_azure_cli_sign_in(
    tenant_id: Option<String>,
    subscription_id: Option<String>,
) -> AzureCliStatus {
    azure_auth::check_azure_cli_sign_in(tenant_id, subscription_id).await
}

#[tauri::command]
pub(crate) fn stop_recording(state: tauri::State<'_, AppState>) -> Result<Meeting, String> {
    let session = state
        .recorder
        .lock()
        .map_err(|_| "Recorder lock poisoned".to_string())?
        .take()
        .ok_or_else(|| "Recording is not running".to_string())?;

    let _ = session.stop_tx.try_send(());
    let _ = session.audio_stop_tx.send(());
    let _ = session.audio_thread.join();

    let samples_16k = {
        let samples = session
            .samples
            .lock()
            .map_err(|_| "Sample buffer lock poisoned".to_string())?;
        mix_sources(&samples.microphone, &samples.system)
    };
    let duration = session.started_at.elapsed().as_secs_f64();
    let recording_path = state
        .data_dir
        .join("recordings")
        .join(format!("{}.wav", session.meeting_id));
    write_wav(&recording_path, &samples_16k, 16_000).map_err(|error| error.to_string())?;
    let persisted_meeting = read_store(&state.data_dir).ok().and_then(|store| {
        store
            .meetings
            .into_iter()
            .find(|meeting| meeting.id == session.meeting_id)
    });

    let updated = {
        let mut store = state
            .store
            .lock()
            .map_err(|_| "Store lock poisoned".to_string())?;
        let meeting = store
            .meetings
            .iter_mut()
            .find(|item| item.id == session.meeting_id)
            .ok_or_else(|| "Meeting not found".to_string())?;
        if let Some(persisted_meeting) = persisted_meeting {
            if persisted_meeting.transcript.len() > meeting.transcript.len() {
                meeting.transcript = persisted_meeting.transcript;
            }
        }
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
pub(crate) fn rename_meeting(
    state: tauri::State<'_, AppState>,
    meeting_id: String,
    title: String,
) -> Result<Meeting, String> {
    let updated = {
        let mut store = state
            .store
            .lock()
            .map_err(|_| "Store lock poisoned".to_string())?;
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
pub(crate) fn update_meeting_language(
    state: tauri::State<'_, AppState>,
    meeting_id: String,
    language: String,
) -> Result<Meeting, String> {
    let language = normalize_language(Some(language));
    let updated = {
        let mut store = state
            .store
            .lock()
            .map_err(|_| "Store lock poisoned".to_string())?;
        let meeting = store
            .meetings
            .iter_mut()
            .find(|item| item.id == meeting_id)
            .ok_or_else(|| "Meeting not found".to_string())?;
        meeting.language = language.clone();
        meeting.updated_at = now_string();
        meeting.clone()
    };
    {
        let recorder = state
            .recorder
            .lock()
            .map_err(|_| "Recorder lock poisoned".to_string())?;
        if let Some(session) = recorder
            .as_ref()
            .filter(|session| session.meeting_id == meeting_id)
        {
            if let Ok(mut current_language) = session.language.lock() {
                *current_language = language;
            }
        }
    }
    state.save_store().map_err(|error| error.to_string())?;
    Ok(updated)
}

#[tauri::command]
pub(crate) fn delete_meeting(
    state: tauri::State<'_, AppState>,
    meeting_id: String,
) -> Result<(), String> {
    let recording_path = {
        let mut store = state
            .store
            .lock()
            .map_err(|_| "Store lock poisoned".to_string())?;
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
        let mut playback = state
            .playback
            .lock()
            .map_err(|_| "Playback lock poisoned".to_string())?;
        if playback
            .as_ref()
            .map(|session| session.meeting_id == meeting_id)
            .unwrap_or(false)
        {
            if let Some(session) = playback.take() {
                stop_playback_session(session);
            }
        }
    }
    state.save_store().map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn export_transcript(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    meeting_id: String,
) -> Result<Option<String>, String> {
    let store = state
        .store
        .lock()
        .map_err(|_| "Store lock poisoned".to_string())?;
    let meeting = find_meeting(&store, &meeting_id).map_err(|error| error.to_string())?;
    let mut text = format!(
        "Meeting: {}\nCreated: {}\nDuration: {}\n\n",
        meeting.title,
        meeting.created_at,
        format_timestamp(meeting.duration_seconds)
    );
    for segment in &meeting.transcript {
        text.push_str(&format!(
            "[{}] {}\n",
            format_timestamp(segment.offset_seconds),
            segment.text
        ));
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
pub(crate) fn export_audio(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    meeting_id: String,
) -> Result<Option<String>, String> {
    let store = state
        .store
        .lock()
        .map_err(|_| "Store lock poisoned".to_string())?;
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
pub(crate) fn open_meeting_folder(
    state: tauri::State<'_, AppState>,
    meeting_id: String,
) -> Result<(), String> {
    let store = state
        .store
        .lock()
        .map_err(|_| "Store lock poisoned".to_string())?;
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
pub(crate) fn play_recording(
    state: tauri::State<'_, AppState>,
    meeting_id: String,
    offset_seconds: Option<f64>,
) -> Result<(), String> {
    let recording_path = {
        let store = state
            .store
            .lock()
            .map_err(|_| "Store lock poisoned".to_string())?;
        let meeting = find_meeting(&store, &meeting_id).map_err(|error| error.to_string())?;
        meeting
            .recording_path
            .ok_or_else(|| "Recording not found".to_string())?
    };

    // rodio's `OutputStream` is `!Send`, so a dedicated thread owns it. The
    // `Sink` is `Send + Sync` and is handed back to control playback from the
    // pause/resume/stop commands without any `unsafe impl Send`.
    let (setup_tx, setup_rx) = std::sync::mpsc::channel::<Result<rodio::Sink, String>>();
    let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
    let thread =
        std::thread::spawn(
            move || match prepare_playback(&recording_path, offset_seconds) {
                Ok((sink, stream)) => {
                    if setup_tx.send(Ok(sink)).is_err() {
                        return;
                    }
                    let _ = stop_rx.recv();
                    drop(stream);
                }
                Err(error) => {
                    let _ = setup_tx.send(Err(error));
                }
            },
        );

    let sink = match setup_rx.recv() {
        Ok(Ok(sink)) => sink,
        Ok(Err(error)) => return Err(error),
        Err(_) => return Err("Playback thread stopped unexpectedly".to_string()),
    };

    let mut playback = state
        .playback
        .lock()
        .map_err(|_| "Playback lock poisoned".to_string())?;
    if let Some(existing) = playback.take() {
        stop_playback_session(existing);
    }
    *playback = Some(PlaybackSession {
        meeting_id,
        sink,
        stop_tx,
        thread,
    });
    Ok(())
}

#[tauri::command]
pub(crate) fn pause_playback(state: tauri::State<'_, AppState>) -> Result<(), String> {
    let playback = state
        .playback
        .lock()
        .map_err(|_| "Playback lock poisoned".to_string())?;
    let Some(playback) = playback.as_ref() else {
        return Err("No active playback".to_string());
    };
    playback.sink.pause();
    Ok(())
}

#[tauri::command]
pub(crate) fn resume_playback(state: tauri::State<'_, AppState>) -> Result<(), String> {
    let playback = state
        .playback
        .lock()
        .map_err(|_| "Playback lock poisoned".to_string())?;
    let Some(playback) = playback.as_ref() else {
        return Err("No active playback".to_string());
    };
    playback.sink.play();
    Ok(())
}

#[tauri::command]
pub(crate) fn set_mini_mode(app: AppHandle, enabled: bool) -> Result<(), String> {
    let Some(window) = app.get_webview_window("main") else {
        return Err("Main window not found".to_string());
    };
    // Relax the minimum size before shrinking so mini mode can actually get
    // small; restore the configured minimum (matching tauri.conf.json) on exit
    // so the full layout, including the recording list, has room again.
    let (min_size, size) = if enabled {
        (
            LogicalSize::new(320.0, 180.0),
            LogicalSize::new(360.0, 200.0),
        )
    } else {
        (
            LogicalSize::new(640.0, 560.0),
            LogicalSize::new(760.0, 760.0),
        )
    };
    window
        .set_min_size(Some(min_size))
        .map_err(|error| error.to_string())?;
    window.set_size(size).map_err(|error| error.to_string())?;
    window
        .set_always_on_top(enabled)
        .map_err(|error| error.to_string())?;
    Ok(())
}

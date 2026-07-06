//! Maintainer guide for Python developers
//!
//! This file is the Rust backend for Meetly Lite. If you usually work in Python,
//! read this block first: it explains what the file implements and how to read
//! the Rust syntax used below. The goal is to make routine maintenance possible
//! without needing any external Rust reference.
//!
//! What this backend implements
//! - Tauri command handlers: functions marked with `#[tauri::command]` are callable
//!   from the React frontend through `invoke(...)`, similar to exposing a Python
//!   function through FastAPI or Flask. For example, `start_recording` is called
//!   from `src/nativeClient.ts`.
//! - Local storage: `Store` is serialized to JSON under the app-data directory.
//!   `read_store` and `write_store` are the closest equivalents to reading and
//!   writing a small Python dict/list structure with `json.load` and `json.dump`.
//! - Meeting records: `Meeting` and `TranscriptSegment` are typed data structures.
//!   Think of them as Python `@dataclass` models, but the compiler checks every
//!   field at build time.
//! - Whisper model loading: `load_whisper_model` loads a local `.bin` Whisper model
//!   into `AppState.whisper`. The model is kept in memory and reused for recordings.
//! - Audio device listing: `list_audio_input_devices` lists microphone/input devices;
//!   `list_audio_output_devices` lists speaker/output devices used for system audio.
//! - Recording modes: `start_recording` supports `microphone`, `system`, and
//!   `microphoneSystem`. The default is simultaneous microphone plus system audio.
//! - Audio processing: incoming audio is converted to `f32`, downmixed to mono,
//!   resampled to 16 kHz, and stored in `CaptureBuffers`. Microphone and system
//!   audio are mixed by `mix_sources` before transcription and WAV export.
//! - Per-session language: each `Meeting` has a `language`. `update_meeting_language`
//!   updates the saved meeting and, if that meeting is currently recording, also
//!   updates the live transcription loop for later chunks.
//! - Live transcripts: while recording, a Tokio task wakes every 5 seconds, drains
//!   pending audio, transcribes it with Whisper, appends it to local JSON storage,
//!   and emits a `transcript-segment` event to the frontend.
//! - Exports and playback: transcripts export as `.txt`, mixed recordings export as
//!   `.wav`, and `rodio` is used for local playback of saved WAV files.
//!
//! Python-to-Rust mental model
//! - `use package::Thing;` is like Python `from package import Thing`.
//! - `struct Meeting { ... }` is like a typed Python dataclass. Field names and
//!   types are fixed; missing fields are compile errors unless `Option<T>` or
//!   `#[serde(default)]` is used.
//! - `Option<T>` means "maybe a value": `Some(value)` is like a present value,
//!   and `None` is like Python `None`. Code must explicitly handle both cases.
//! - `Result<T, E>` means "success or error": `Ok(value)` is success and `Err(error)`
//!   is failure. The `?` operator returns early on error, similar to raising an
//!   exception, but the function signature must say it can return a `Result`.
//! - Rust has ownership. Passing `value` can move it; passing `&value` borrows it.
//!   If Python intuition says "I just want to read this without taking it", expect
//!   Rust code to use `&T` or `&str`.
//! - `.clone()` makes an owned copy or increments a reference-counted handle. It is
//!   used here when a value must be kept in app state and also moved into an async
//!   task or closure.
//! - `Arc<T>` is an atomic reference-counted pointer: it lets multiple threads/tasks
//!   share the same value. Think of it as a safe shared handle.
//! - `Mutex<T>` protects mutable shared state. To access the value, call `.lock()`;
//!   this returns a guard. The lock is released when the guard goes out of scope.
//! - `Arc<Mutex<T>>` is the common "shared mutable state across async tasks" pattern
//!   used here for audio buffers and current recording language.
//! - `let mut x = ...` means `x` can be reassigned or mutated. Without `mut`, Rust
//!   variables are immutable by default.
//! - `match value { ... }` is a structured `if/elif` for enums and patterns. It is
//!   heavily used for audio sample formats and `Option`/`Result` handling.
//! - `if let Some(value) = maybe_value { ... }` is shorthand for "run this block only
//!   when the optional value exists".
//! - Closures look like `move |data, info| { ... }`. These are like Python lambdas or
//!   nested functions, but they can capture surrounding variables. `move` transfers
//!   captured values into the closure so audio callbacks can outlive this function.
//! - `tokio::spawn(async move { ... })` starts an async background task, similar in
//!   spirit to `asyncio.create_task(...)`. Anything used inside must be owned by the
//!   task or wrapped in shared handles such as `Arc`.
//! - `drop(value)` explicitly releases a value early. Here it stops audio streams or
//!   releases locks/files before later work continues.
//! - `#[derive(Serialize, Deserialize)]` asks Rust to generate JSON serialization
//!   code. `#[serde(rename_all = "camelCase")]` makes Rust fields like
//!   `audio_device_id` appear as `audioDeviceId` in TypeScript.
//!
//! How audio recording flows through this file
//! 1. The frontend calls `list_audio_input_devices` and `list_audio_output_devices`
//!    so the user can choose microphone and system audio sources.
//! 2. The frontend calls `start_recording` with the selected device IDs, capture
//!    mode, and language.
//! 3. `start_recording` resolves the selected CPAL devices. For system audio it uses
//!    the Windows WASAPI host and output devices, matching the reference app's idea
//!    of treating speaker/output devices as system-audio capture sources.
//! 4. `build_capture_stream` creates one stream per requested source. Each callback
//!    converts raw samples to mono 16 kHz `f32` samples and appends them to both the
//!    full recording buffer and the pending transcription buffer.
//! 5. A background Tokio task drains pending buffers every 5 seconds, mixes microphone
//!    and system samples, reads the current session language, and calls Whisper.
//! 6. `stop_recording` stops all active streams, mixes the full buffers, writes one
//!    WAV file, and updates the meeting metadata.
//!
//! Maintenance tips
//! - If changing frontend command names or argument names, update both this file and
//!   `src/nativeClient.ts`; Tauri maps camelCase TypeScript arguments to snake_case
//!   Rust parameters.
//! - If adding fields to `Meeting`, add `#[serde(default)]` when old stored JSON files
//!   should continue loading without that field.
//! - Keep audio callback work small. Heavy work belongs in the Tokio task, not inside
//!   the CPAL callback, because callbacks run on the audio thread.
//! - Prefer returning `Result<..., String>` from Tauri commands so frontend errors are
//!   readable.
//! - This app is local-first. Do not add network calls for audio, transcripts, or
//!   model processing unless the product scope changes explicitly.
//!
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
    #[serde(default)]
    pub audio_device_id: Option<String>,
    #[serde(default)]
    pub audio_device_name: Option<String>,
    #[serde(default)]
    pub system_audio_device_id: Option<String>,
    #[serde(default)]
    pub system_audio_device_name: Option<String>,
    #[serde(default = "default_capture_mode")]
    pub capture_mode: String,
    #[serde(default = "default_language")]
    pub language: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioInputDevice {
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioOutputDevice {
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WhisperModelStatus {
    pub loaded: bool,
    pub path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptEvent {
    pub meeting_id: String,
    pub segment: TranscriptSegment,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Store {
    meetings: Vec<Meeting>,
    whisper_model_path: Option<String>,
}

struct RecorderSession {
    meeting_id: String,
    stop_tx: mpsc::Sender<()>,
    audio_stop_tx: std::sync::mpsc::Sender<()>,
    audio_thread: std::thread::JoinHandle<()>,
    samples: Arc<Mutex<CaptureBuffers>>,
    language: Arc<Mutex<String>>,
    started_at: std::time::Instant,
}

#[derive(Default)]
struct CaptureBuffers {
    microphone: Vec<f32>,
    system: Vec<f32>,
}

#[derive(Clone, Copy)]
enum AudioSource {
    Microphone,
    System,
}

/// Devices and capture mode resolved on the audio thread, returned to
/// `start_recording` so it can persist the meeting metadata. Every field is
/// `Send`, unlike the CPAL streams that stay behind on the audio thread.
#[derive(Default)]
struct AudioSetup {
    resolved_audio_device_id: Option<String>,
    audio_device_name: Option<String>,
    resolved_system_audio_device_id: Option<String>,
    system_audio_device_name: Option<String>,
    resolved_capture_mode: String,
}

struct PlaybackSession {
    meeting_id: String,
    sink: Sink,
    stop_tx: std::sync::mpsc::Sender<()>,
    thread: std::thread::JoinHandle<()>,
}

struct AppState {
    store: Mutex<Store>,
    recorder: Mutex<Option<RecorderSession>>,
    playback: Mutex<Option<PlaybackSession>>,
    whisper: Mutex<Option<Arc<WhisperContext>>>,
    data_dir: PathBuf,
}

impl AppState {
    fn new(app: &tauri::App) -> Result<Self> {
        let data_dir = app.path().app_data_dir().unwrap_or_else(|_| {
            dirs::data_local_dir()
                .unwrap_or_else(std::env::temp_dir)
                .join("MeetlyLite")
        });
        fs::create_dir_all(data_dir.join("recordings"))?;
        let mut store = read_store(&data_dir)?;

        // Auto-detect a local Whisper model when none is configured (or the saved
        // path no longer exists) so recording can start without manual setup.
        let has_valid_model = store
            .whisper_model_path
            .as_ref()
            .map(|path| Path::new(path).is_file())
            .unwrap_or(false);
        if !has_valid_model {
            if let Some(model_path) = find_default_model(&model_search_dirs(app, &data_dir)) {
                store.whisper_model_path = Some(model_path.to_string_lossy().to_string());
                let _ = write_store(&data_dir, &store);
            }
        }

        Ok(Self {
            store: Mutex::new(store),
            recorder: Mutex::new(None),
            playback: Mutex::new(None),
            whisper: Mutex::new(None),
            data_dir,
        })
    }

    fn save_store(&self) -> Result<()> {
        let store = self
            .store
            .lock()
            .map_err(|_| anyhow!("Store lock poisoned"))?;
        write_store(&self.data_dir, &store)
    }
}

fn store_path(data_dir: &Path) -> PathBuf {
    data_dir.join("store.json")
}

/// Directories to probe when auto-detecting a bundled or local Whisper model.
/// Covers the packaged resource dir, the app data dir, and the executable and
/// working-directory trees so development and installed builds both work.
fn model_search_dirs(app: &tauri::App, data_dir: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(resource_dir) = app.path().resource_dir() {
        dirs.push(resource_dir.join("models"));
        dirs.push(resource_dir);
    }
    dirs.extend(fallback_model_dirs(data_dir));
    dirs
}

/// Model search directories that do not require an `App` handle, so they can be
/// used both at startup and lazily when a recording starts.
fn fallback_model_dirs(data_dir: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![data_dir.join("models")];

    if let Ok(exe) = std::env::current_exe() {
        let mut base = exe.parent().map(Path::to_path_buf);
        for _ in 0..5 {
            let Some(dir) = base else { break };
            dirs.push(dir.join("models"));
            base = dir.parent().map(Path::to_path_buf);
        }
    }

    if let Ok(cwd) = std::env::current_dir() {
        dirs.push(cwd.join("models"));
        if let Some(parent) = cwd.parent() {
            dirs.push(parent.join("models"));
        }
    }

    dirs
}

/// Returns the first `.bin` Whisper model found in the given directories,
/// preferring files whose name starts with `ggml`.
fn find_default_model(dirs: &[PathBuf]) -> Option<PathBuf> {
    for dir in dirs {
        let Ok(entries) = fs::read_dir(dir) else {
            continue;
        };
        let mut models: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.is_file()
                    && path.extension().and_then(|ext| ext.to_str()) == Some("bin")
            })
            .collect();
        if models.is_empty() {
            continue;
        }
        models.sort();
        let preferred = models
            .iter()
            .find(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .map(|name| name.starts_with("ggml"))
                    .unwrap_or(false)
            })
            .cloned();
        return preferred.or_else(|| models.into_iter().next());
    }
    None
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

fn default_language() -> String {
    "en".to_string()
}

fn default_capture_mode() -> String {
    "microphoneSystem".to_string()
}

const TRANSCRIPT_CHUNK_SECONDS: u64 = 5;
const TRANSCRIPT_SAMPLE_RATE: f64 = 16_000.0;

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

fn downmix_to_mono(samples: &[f32], channels: u16) -> Vec<f32> {
    let channel_count = usize::from(channels.max(1));
    if channel_count == 1 {
        return samples.to_vec();
    }
    samples
        .chunks(channel_count)
        .map(|frame| frame.iter().copied().sum::<f32>() / frame.len() as f32)
        .collect()
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

fn mix_sources(microphone: &[f32], system: &[f32]) -> Vec<f32> {
    if microphone.is_empty() {
        return system.to_vec();
    }
    if system.is_empty() {
        return microphone.to_vec();
    }
    let max_len = microphone.len().max(system.len());
    let mut mixed = Vec::with_capacity(max_len);
    for index in 0..max_len {
        let mic_sample = microphone.get(index).copied().unwrap_or_default();
        let system_sample = system.get(index).copied().unwrap_or_default();
        mixed.push((mic_sample * 0.6 + system_sample * 0.4).clamp(-1.0, 1.0));
    }
    mixed
}

fn append_capture_samples(
    buffers: &Arc<Mutex<CaptureBuffers>>,
    source: AudioSource,
    samples: &[f32],
) {
    if let Ok(mut buffers) = buffers.lock() {
        match source {
            AudioSource::Microphone => buffers.microphone.extend_from_slice(samples),
            AudioSource::System => buffers.system.extend_from_slice(samples),
        }
    }
}

fn convert_samples_i16(samples: &[i16]) -> Vec<f32> {
    samples
        .iter()
        .map(|sample| *sample as f32 / i16::MAX as f32)
        .collect()
}

fn convert_samples_u16(samples: &[u16]) -> Vec<f32> {
    samples
        .iter()
        .map(|sample| (*sample as f32 / u16::MAX as f32) * 2.0 - 1.0)
        .collect()
}

fn build_capture_stream(
    device: &cpal::Device,
    config: cpal::SupportedStreamConfig,
    source: AudioSource,
    samples: Arc<Mutex<CaptureBuffers>>,
    pending: Arc<Mutex<CaptureBuffers>>,
) -> Result<cpal::Stream, String> {
    let sample_rate = config.sample_rate().0;
    let channels = config.channels();
    let error_callback = |error| eprintln!("Audio stream error: {error}");

    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => device.build_input_stream(
            &config.into(),
            move |data: &[f32], _| {
                let mono = downmix_to_mono(data, channels);
                let samples_16k = resample_to_16khz(&mono, sample_rate);
                append_capture_samples(&samples, source, &samples_16k);
                append_capture_samples(&pending, source, &samples_16k);
            },
            error_callback,
            None,
        ),
        cpal::SampleFormat::I16 => device.build_input_stream(
            &config.into(),
            move |data: &[i16], _| {
                let converted = convert_samples_i16(data);
                let mono = downmix_to_mono(&converted, channels);
                let samples_16k = resample_to_16khz(&mono, sample_rate);
                append_capture_samples(&samples, source, &samples_16k);
                append_capture_samples(&pending, source, &samples_16k);
            },
            error_callback,
            None,
        ),
        cpal::SampleFormat::U16 => device.build_input_stream(
            &config.into(),
            move |data: &[u16], _| {
                let converted = convert_samples_u16(data);
                let mono = downmix_to_mono(&converted, channels);
                let samples_16k = resample_to_16khz(&mono, sample_rate);
                append_capture_samples(&samples, source, &samples_16k);
                append_capture_samples(&pending, source, &samples_16k);
            },
            error_callback,
            None,
        ),
        _ => return Err("Unsupported audio sample format".to_string()),
    }
    .map_err(|error| error.to_string())?;

    Ok(stream)
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

#[tauri::command]
fn get_meetings(state: tauri::State<'_, AppState>) -> Result<Vec<Meeting>, String> {
    let store = state
        .store
        .lock()
        .map_err(|_| "Store lock poisoned".to_string())?;
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
fn get_whisper_model_status(
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
            let detected = find_default_model(&fallback_model_dirs(&state.data_dir))
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
fn select_whisper_model(app: AppHandle) -> Result<Option<String>, String> {
    let selected = app
        .dialog()
        .file()
        .add_filter("Whisper ggml model", &["bin"])
        .blocking_pick_file();

    Ok(selected.map(|path| path.to_string()))
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

fn audio_host() -> cpal::Host {
    cpal::host_from_id(cpal::HostId::Wasapi).unwrap_or_else(|_| cpal::default_host())
}

fn default_audio_input_device(host: &cpal::Host) -> Result<cpal::Device, String> {
    host.default_input_device()
        .ok_or_else(|| "No default microphone found".to_string())
}

fn default_audio_output_device(host: &cpal::Host) -> Result<cpal::Device, String> {
    host.default_output_device()
        .ok_or_else(|| "No default system audio output found".to_string())
}

fn audio_device_name(device: &cpal::Device) -> String {
    device
        .name()
        .unwrap_or_else(|_| "Unknown input".to_string())
}

fn resolve_audio_input_device(
    host: &cpal::Host,
    audio_device_id: Option<&str>,
) -> Result<(cpal::Device, Option<String>, Option<String>), String> {
    let selected_id = audio_device_id
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if let Some(selected_id) = selected_id {
        let devices = host.input_devices().map_err(|error| error.to_string())?;
        for device in devices {
            let name = audio_device_name(&device);
            if name == selected_id {
                return Ok((device, Some(selected_id.to_string()), Some(name)));
            }
        }
    }

    let device = default_audio_input_device(host)?;
    let name = audio_device_name(&device);
    Ok((device, None, Some(name)))
}

fn resolve_audio_output_device(
    host: &cpal::Host,
    system_audio_device_id: Option<&str>,
) -> Result<(cpal::Device, Option<String>, Option<String>), String> {
    let selected_id = system_audio_device_id
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if let Some(selected_id) = selected_id {
        let devices = host.output_devices().map_err(|error| error.to_string())?;
        for device in devices {
            let name = audio_device_name(&device);
            if name == selected_id {
                return Ok((device, Some(selected_id.to_string()), Some(name)));
            }
        }
    }

    let device = default_audio_output_device(host)?;
    let name = audio_device_name(&device);
    Ok((device, None, Some(name)))
}

#[tauri::command]
fn list_audio_input_devices() -> Result<Vec<AudioInputDevice>, String> {
    let host = audio_host();
    let default_name = host
        .default_input_device()
        .map(|device| audio_device_name(&device));
    let mut devices = vec![AudioInputDevice {
        id: String::new(),
        name: "System default".to_string(),
        is_default: true,
    }];

    for device in host.input_devices().map_err(|error| error.to_string())? {
        let name = audio_device_name(&device);
        if devices.iter().any(|item| item.id == name) {
            continue;
        }
        let is_default = default_name.as_deref() == Some(name.as_str());
        devices.push(AudioInputDevice {
            id: name.clone(),
            name,
            is_default,
        });
    }

    Ok(devices)
}

#[tauri::command]
fn list_audio_output_devices() -> Result<Vec<AudioOutputDevice>, String> {
    let host = audio_host();
    let default_name = host
        .default_output_device()
        .map(|device| audio_device_name(&device));
    let mut devices = vec![AudioOutputDevice {
        id: String::new(),
        name: "System default".to_string(),
        is_default: true,
    }];

    for device in host.output_devices().map_err(|error| error.to_string())? {
        let name = audio_device_name(&device);
        if devices.iter().any(|item| item.id == name) {
            continue;
        }
        let is_default = default_name.as_deref() == Some(name.as_str());
        devices.push(AudioOutputDevice {
            id: name.clone(),
            name,
            is_default,
        });
    }

    Ok(devices)
}

/// Builds and starts the CPAL capture streams for the requested sources.
///
/// This runs entirely on the audio thread because `cpal::Stream` is `!Send`;
/// the returned streams must be kept alive and dropped on the same thread.
fn build_capture_streams(
    capture_microphone: bool,
    capture_system: bool,
    audio_device_id: Option<&str>,
    system_audio_device_id: Option<&str>,
    capture_mode: &str,
    samples: Arc<Mutex<CaptureBuffers>>,
    pending: Arc<Mutex<CaptureBuffers>>,
) -> Result<(Vec<cpal::Stream>, AudioSetup), String> {
    let host = audio_host();
    let mut streams = Vec::new();
    let mut setup = AudioSetup {
        resolved_capture_mode: capture_mode.to_string(),
        ..AudioSetup::default()
    };

    if capture_microphone {
        let (device, device_id, device_name) = resolve_audio_input_device(&host, audio_device_id)?;
        let config = device
            .default_input_config()
            .map_err(|error| error.to_string())?;
        let stream = build_capture_stream(
            &device,
            config,
            AudioSource::Microphone,
            samples.clone(),
            pending.clone(),
        )?;
        stream.play().map_err(|error| error.to_string())?;
        streams.push(stream);
        setup.resolved_audio_device_id = device_id;
        setup.audio_device_name = device_name;
    }

    if capture_system {
        match resolve_audio_output_device(&host, system_audio_device_id).and_then(
            |(device, device_id, device_name)| {
                let config = device
                    .default_output_config()
                    .map_err(|error| error.to_string())?;
                let stream = build_capture_stream(
                    &device,
                    config,
                    AudioSource::System,
                    samples.clone(),
                    pending.clone(),
                )?;
                stream.play().map_err(|error| error.to_string())?;
                Ok((stream, device_id, device_name))
            },
        ) {
            Ok((stream, device_id, device_name)) => {
                streams.push(stream);
                setup.resolved_system_audio_device_id = device_id;
                setup.system_audio_device_name = device_name;
            }
            Err(error) if capture_microphone => {
                eprintln!("System audio capture could not start, continuing with microphone only: {error}");
                setup.resolved_capture_mode = "microphone".to_string();
            }
            Err(error) => return Err(format!("System audio capture could not start: {error}")),
        }
    }

    if streams.is_empty() {
        return Err("No audio capture stream could be started".to_string());
    }

    Ok((streams, setup))
}

#[tauri::command]
fn start_recording(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    meeting_title: Option<String>,
    audio_device_id: Option<String>,
    system_audio_device_id: Option<String>,
    capture_mode: Option<String>,
    language: Option<String>,
) -> Result<Meeting, String> {
    if state
        .recorder
        .lock()
        .map_err(|_| "Recorder lock poisoned".to_string())?
        .is_some()
    {
        return Err("Recording already in progress".to_string());
    }

    let context = get_or_load_whisper_context(&state)?;

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
    let (setup_tx, setup_rx) = std::sync::mpsc::channel::<Result<AudioSetup, String>>();
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
        let mut interval = tokio::time::interval(Duration::from_secs(TRANSCRIPT_CHUNK_SECONDS));
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
                    let chunk_language = language_for_task.lock().map(|value| value.clone()).unwrap_or_else(|_| default_language());
                    let context_for_chunk = context.clone();
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

fn append_segment_to_store(
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
fn stop_recording(state: tauri::State<'_, AppState>) -> Result<Meeting, String> {
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
fn rename_meeting(
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
fn update_meeting_language(
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
fn delete_meeting(state: tauri::State<'_, AppState>, meeting_id: String) -> Result<(), String> {
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
fn export_transcript(
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
fn export_audio(
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
fn open_meeting_folder(
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

/// Opens the recording, wires up a rodio `Sink`, and starts playback.
///
/// Runs on the playback thread because `OutputStream` is `!Send`; the stream is
/// returned alongside the `Sink` so the caller can keep it alive on that thread.
fn prepare_playback(
    recording_path: &str,
    offset_seconds: Option<f64>,
) -> Result<(Sink, OutputStream), String> {
    let file = fs::File::open(recording_path).map_err(|error| error.to_string())?;
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
    Ok((sink, stream))
}

/// Stops a playback session and tears down its `OutputStream` on the owning thread.
fn stop_playback_session(session: PlaybackSession) {
    session.sink.stop();
    let _ = session.stop_tx.send(());
    let _ = session.thread.join();
}

#[tauri::command]
fn play_recording(
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
    let (setup_tx, setup_rx) = std::sync::mpsc::channel::<Result<Sink, String>>();
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
fn pause_playback(state: tauri::State<'_, AppState>) -> Result<(), String> {
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
fn resume_playback(state: tauri::State<'_, AppState>) -> Result<(), String> {
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
    window
        .set_always_on_top(enabled)
        .map_err(|error| error.to_string())?;
    Ok(())
}

pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let state = AppState::new(app)
                .map_err(|error| Box::<dyn std::error::Error>::from(error.to_string()))?;
            app.manage(state);
            Ok(())
        })
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            get_meetings,
            list_audio_input_devices,
            list_audio_output_devices,
            load_whisper_model,
            select_whisper_model,
            get_whisper_model_status,
            start_recording,
            stop_recording,
            rename_meeting,
            update_meeting_language,
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
        .expect("error while running Meetly Lite");
}

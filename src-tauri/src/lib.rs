mod audio;
mod azure_auth;
mod commands;
mod video;

#[cfg(test)]
mod test;

use anyhow::{anyhow, Context, Result};
use audio::{CaptureBuffers, IncrementalWavWriter};
use rodio::Sink;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tauri::Manager;
use tokio::sync::mpsc;
use whisper_rs::WhisperContext;

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
    #[serde(default = "default_transcription_engine")]
    pub transcription_engine: String,
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
pub struct VideoRecording {
    pub id: String,
    pub title: String,
    pub created_at: String,
    pub updated_at: String,
    pub duration_seconds: f64,
    pub status: String,
    pub target_name: String,
    pub codec: String,
    pub video_path: String,
    #[serde(default)]
    pub has_audio: Option<bool>,
    #[serde(default)]
    pub audio_path: Option<String>,
    #[serde(default)]
    pub post_process_stage: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptEvent {
    pub meeting_id: String,
    pub segment: TranscriptSegment,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioChunkEvent {
    pub meeting_id: String,
    pub offset_seconds: f64,
    pub pcm_base64: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreenAudioLevelEvent {
    pub microphone: f32,
    pub system: f32,
    pub mixed: f32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreenProcessingStatusEvent {
    pub stage: String,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingRuntimeStatus {
    pub audio_recording_active: bool,
    pub audio_meeting_id: Option<String>,
    pub audio_elapsed_seconds: f64,
    pub screen_recording_active: bool,
    pub screen_video_id: Option<String>,
    pub screen_elapsed_seconds: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TranscriptionEngine {
    Local,
    Azure,
}

/// User-facing settings persisted across app restarts. All fields default to an
/// empty string; the frontend applies its own defaults for blank values.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AppSettings {
    pub transcription_engine: String,
    pub capture_mode: String,
    pub screen_audio_capture_mode: String,
    pub audio_device_id: String,
    pub system_audio_device_id: String,
    pub language: String,
    pub azure_endpoint: String,
    pub azure_tenant_id: String,
    pub azure_subscription_id: String,
    pub azure_language: String,
    pub video_output_folder: String,
    pub video_codec: String,
    pub ffmpeg_path: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Store {
    pub(crate) meetings: Vec<Meeting>,
    #[serde(default)]
    pub(crate) videos: Vec<VideoRecording>,
    pub(crate) whisper_model_path: Option<String>,
    #[serde(default)]
    pub(crate) settings: AppSettings,
}

pub(crate) struct RecorderSession {
    pub(crate) meeting_id: String,
    pub(crate) prior_recording_path: Option<PathBuf>,
    pub(crate) stop_tx: mpsc::Sender<()>,
    pub(crate) audio_stop_tx: std::sync::mpsc::Sender<()>,
    pub(crate) audio_thread: std::thread::JoinHandle<()>,
    pub(crate) audio_done_rx: std::sync::mpsc::Receiver<()>,
    pub(crate) recording_samples: Arc<Mutex<CaptureBuffers>>,
    pub(crate) audio_writer: Arc<Mutex<Option<IncrementalWavWriter>>>,
    pub(crate) recording_path: PathBuf,
    pub(crate) language: Arc<Mutex<String>>,
    pub(crate) started_at: std::time::Instant,
}

pub(crate) struct PlaybackSession {
    pub(crate) meeting_id: String,
    pub(crate) sink: Sink,
    pub(crate) stop_tx: std::sync::mpsc::Sender<()>,
    pub(crate) thread: std::thread::JoinHandle<()>,
}

pub(crate) struct ScreenRecorderSession {
    pub(crate) video_id: String,
    pub(crate) capture: video::ScreenCaptureProcess,
    pub(crate) audio_stop_tx: Option<std::sync::mpsc::Sender<()>>,
    pub(crate) audio_thread: Option<std::thread::JoinHandle<()>>,
    pub(crate) started_at: std::time::Instant,
}

pub(crate) struct AppState {
    pub(crate) store: Mutex<Store>,
    pub(crate) recording_lifecycle_lock: Mutex<()>,
    pub(crate) recorder: Mutex<Option<RecorderSession>>,
    pub(crate) screen_recorder: Mutex<Option<ScreenRecorderSession>>,
    pub(crate) playback: Mutex<Option<PlaybackSession>>,
    pub(crate) whisper: Mutex<Option<Arc<WhisperContext>>>,
    pub(crate) data_dir: PathBuf,
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
            recording_lifecycle_lock: Mutex::new(()),
            recorder: Mutex::new(None),
            screen_recorder: Mutex::new(None),
            playback: Mutex::new(None),
            whisper: Mutex::new(None),
            data_dir,
        })
    }

    pub(crate) fn save_store(&self) -> Result<()> {
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
pub(crate) fn fallback_model_dirs(data_dir: &Path) -> Vec<PathBuf> {
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
pub(crate) fn find_default_model(dirs: &[PathBuf]) -> Option<PathBuf> {
    for dir in dirs {
        let Ok(entries) = fs::read_dir(dir) else {
            continue;
        };
        let mut models: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.is_file() && path.extension().and_then(|ext| ext.to_str()) == Some("bin")
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

pub(crate) fn read_store(data_dir: &Path) -> Result<Store> {
    let path = store_path(data_dir);
    if !path.exists() {
        let store = Store::default();
        write_store(data_dir, &store)?;
        return Ok(store);
    }
    let raw =
        fs::read_to_string(&path).with_context(|| format!("Could not read {}", path.display()))?;
    Ok(serde_json::from_str(&raw).unwrap_or_default())
}

pub(crate) fn write_store(data_dir: &Path, store: &Store) -> Result<()> {
    fs::create_dir_all(data_dir)?;
    let path = store_path(data_dir);
    let serialized = serde_json::to_string_pretty(store)?;
    fs::write(&path, serialized).with_context(|| format!("Could not write {}", path.display()))
}

pub(crate) fn default_language() -> String {
    "en".to_string()
}

pub(crate) fn default_capture_mode() -> String {
    "microphoneSystem".to_string()
}

pub(crate) fn default_transcription_engine() -> String {
    "local".to_string()
}

pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let state = AppState::new(app)
                .map_err(|error| Box::<dyn std::error::Error>::from(error.to_string()))?;
            app.manage(state);
            let app_handle = app.handle().clone();
            tauri::async_runtime::spawn_blocking(move || {
                let state = app_handle.state::<AppState>();
                let lifecycle_guard = state.recording_lifecycle_lock.lock();
                match lifecycle_guard {
                    Ok(_guard) => {
                        if let Err(error) = commands::screen::recover_incomplete_screen_recordings(
                            &app_handle,
                            &state,
                        ) {
                            eprintln!("Could not recover incomplete screen recordings: {error}");
                        }
                        if let Err(error) = commands::meetings::recover_incomplete_audio_encodings(
                            &app_handle,
                            &state,
                        ) {
                            eprintln!("Could not recover incomplete audio encodings: {error}");
                        }
                    }
                    Err(_) => eprintln!("Could not lock screen recording recovery state."),
                }
            });
            Ok(())
        })
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            commands::get_meetings,
            commands::refresh_meetings,
            commands::screen::get_videos,
            commands::screen::refresh_videos,
            commands::screen::delete_video,
            commands::screen::get_recording_runtime_status,
            commands::meetings::list_audio_input_devices,
            commands::meetings::list_audio_output_devices,
            commands::screen::list_screen_targets,
            commands::screen::open_area_selector,
            commands::screen::complete_area_selection,
            commands::screen::cancel_area_selection,
            commands::screen::select_video_output_folder,
            commands::screen::select_ffmpeg_executable,
            commands::screen::open_video_recordings_folder,
            commands::meetings::load_whisper_model,
            commands::meetings::select_whisper_model,
            commands::meetings::get_whisper_model_status,
            commands::get_settings,
            commands::save_settings,
            commands::meetings::start_recording,
            commands::meetings::stop_recording,
            commands::screen::start_screen_recording,
            commands::screen::stop_screen_recording,
            commands::transcription::select_fast_transcription_audio,
            commands::transcription::transcribe_fast_audio,
            commands::meetings::add_transcript_segment,
            commands::meetings::get_azure_cli_access_token,
            commands::meetings::sign_in_azure_cli,
            commands::meetings::check_azure_cli_sign_in,
            commands::meetings::rename_meeting,
            commands::meetings::update_meeting_language,
            commands::delete_meeting,
            commands::export_transcript,
            commands::export_audio,
            commands::open_meeting_folder,
            commands::play_recording,
            commands::pause_playback,
            commands::resume_playback,
            commands::set_mini_mode,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Meetly Lite");
}

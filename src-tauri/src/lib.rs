mod audio;
mod azure_auth;
mod commands;
mod copilot;
mod speakers;
mod foundry_local;
mod speech_gate;
mod video;
mod windows_call_mute;

#[cfg(test)]
mod test;

use anyhow::{anyhow, Context, Result};
use audio::IncrementalWavWriter;
use rodio::Sink;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tauri::Manager;
use tokio::sync::mpsc;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptSegment {
    pub id: String,
    pub offset_seconds: f64,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speaker_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speaker_name: Option<String>,
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
    pub audio_microphone_muted: bool,
    pub screen_recording_active: bool,
    pub screen_video_id: Option<String>,
    pub screen_elapsed_seconds: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TranscriptionEngine {
    Azure,
    FoundryLocal,
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
    pub foundry_local_model_alias: String,
    pub foundry_local_language: String,
    pub foundry_local_chunking_mode: String,
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
    #[serde(default)]
    pub(crate) settings: AppSettings,
}

pub(crate) struct RecorderSession {
    pub(crate) meeting_id: String,
    pub(crate) prior_recording_path: Option<PathBuf>,
    pub(crate) base_offset_seconds: f64,
    pub(crate) stop_tx: mpsc::Sender<()>,
    pub(crate) transcription_done_rx: tokio::sync::oneshot::Receiver<Result<(), String>>,
    pub(crate) recording_stop_tx: mpsc::Sender<()>,
    pub(crate) recording_done_rx: tokio::sync::oneshot::Receiver<Result<(), String>>,
    pub(crate) audio_stop_tx: std::sync::mpsc::Sender<()>,
    pub(crate) audio_thread: std::thread::JoinHandle<()>,
    pub(crate) audio_done_rx: std::sync::mpsc::Receiver<()>,
    pub(crate) audio_writer: Arc<Mutex<Option<IncrementalWavWriter>>>,
    pub(crate) recording_path: PathBuf,
    pub(crate) language: Arc<Mutex<String>>,
    pub(crate) microphone_muted: Arc<std::sync::atomic::AtomicBool>,
    pub(crate) teams_mute_monitor: Option<windows_call_mute::TeamsMuteMonitor>,
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
        let store = read_store(&data_dir)?;

        Ok(Self {
            store: Mutex::new(store),
            recording_lifecycle_lock: Mutex::new(()),
            recorder: Mutex::new(None),
            screen_recorder: Mutex::new(None),
            playback: Mutex::new(None),
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
    "foundryLocal".to_string()
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
            copilot::copilot_status,
            copilot::get_meeting_assistant,
            copilot::clear_meeting_assistant,
            copilot::ask_copilot,
            copilot::set_action_item_done,
            speakers::rename_speaker,
            commands::get_meetings,
            commands::refresh_meetings,
            commands::screen::get_videos,
            commands::screen::refresh_videos,
            commands::screen::delete_video,
            commands::screen::rename_video,
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
            commands::get_settings,
            commands::save_settings,
            commands::meetings::list_foundry_local_models,
            commands::meetings::download_foundry_local_model,
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

use crate::audio::{
    append_wav_chunk, build_capture_streams, create_incremental_wav, list_input_devices,
    list_output_devices, mix_sources, pcm16_base64, prepare_playback, stop_playback_session,
    CaptureBuffers,
};
use crate::azure_auth::{self, AzureCliAccessToken, AzureCliStatus};
use crate::video::{self, ScreenTarget};
use crate::{
    default_capture_mode, default_language, read_store, write_store, AppSettings, AppState,
    AudioChunkEvent, AudioInputDevice, AudioOutputDevice, Meeting, PlaybackSession,
    RecorderSession, RecordingRuntimeStatus, ScreenAudioLevelEvent, ScreenProcessingStatusEvent,
    ScreenRecorderSession, Store, TranscriptEvent, TranscriptSegment, VideoRecording,
};
use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use rodio::{Decoder, Source};
use std::fs;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{
    AppHandle, Emitter, LogicalSize, Manager, PhysicalPosition, PhysicalSize, WebviewUrl,
    WebviewWindowBuilder,
};
use tauri_plugin_dialog::DialogExt;
use tokio::sync::mpsc;
use uuid::Uuid;

pub(crate) mod meetings;
pub(crate) mod screen;
pub(crate) mod transcription;

const TRANSCRIPT_SAMPLE_RATE: f64 = 16_000.0;
const AUDIO_DEVICE_SETUP_TIMEOUT: Duration = Duration::from_secs(10);
const AUDIO_THREAD_STOP_TIMEOUT: Duration = Duration::from_secs(5);
type ScreenAudioCaptureHandle = (std::sync::mpsc::Sender<()>, std::thread::JoinHandle<()>);
const FAST_TRANSCRIPTION_MAX_BYTES: u64 = 250 * 1024 * 1024;
const FAST_TRANSCRIPTION_MAX_DURATION: Duration = Duration::from_secs(2 * 60 * 60);

fn now_string() -> String {
    Utc::now().to_rfc3339()
}

fn format_timestamp(seconds: f64) -> String {
    let total = seconds.max(0.0).floor() as u64;
    format!("{:02}:{:02}", total / 60, total % 60)
}

struct MeetingOptions {
    title: Option<String>,
    audio_device_id: Option<String>,
    audio_device_name: Option<String>,
    system_audio_device_id: Option<String>,
    system_audio_device_name: Option<String>,
    capture_mode: String,
    language: String,
    transcription_engine: String,
}

fn make_meeting(options: MeetingOptions) -> Meeting {
    let now = now_string();
    Meeting {
        id: format!("meeting-{}", Uuid::new_v4()),
        title: options.title.unwrap_or_else(|| {
            format!("Meeting {}", chrono::Local::now().format("%Y-%m-%d %H-%M"))
        }),
        created_at: now.clone(),
        updated_at: now,
        duration_seconds: 0.0,
        transcript: Vec::new(),
        has_audio: false,
        recording_path: None,
        audio_device_id: options.audio_device_id,
        audio_device_name: options.audio_device_name,
        system_audio_device_id: options.system_audio_device_id,
        system_audio_device_name: options.system_audio_device_name,
        capture_mode: options.capture_mode,
        language: options.language,
        transcription_engine: options.transcription_engine,
    }
}

fn fast_transcription_file_details(path: &Path) -> Result<(String, u64, Duration), String> {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
        .ok_or_else(|| "Choose a WAV, MP3, or MP4 file.".to_string())?;
    if extension != "wav" && extension != "mp3" && extension != "mp4" {
        return Err("Fast transcription supports WAV, MP3, and MP4 files only.".to_string());
    }
    let metadata = fs::metadata(path).map_err(|error| error.to_string())?;
    if metadata.len() == 0 {
        return Err("The selected audio file is empty.".to_string());
    }
    if metadata.len() > FAST_TRANSCRIPTION_MAX_BYTES {
        return Err(
            "The selected file exceeds the 250 MB Azure Speech Fast Transcription limit."
                .to_string(),
        );
    }
    let duration = if extension == "mp4" {
        Duration::ZERO
    } else {
        let file = fs::File::open(path).map_err(|error| error.to_string())?;
        Decoder::new(BufReader::new(file))
            .map_err(|error| format!("Could not read the selected audio file: {error}"))?
            .total_duration()
            .ok_or_else(|| "Could not determine the selected audio duration.".to_string())?
    };
    if duration > FAST_TRANSCRIPTION_MAX_DURATION {
        return Err(
            "The selected audio is longer than the 2 hour Azure Speech Fast Transcription limit."
                .to_string(),
        );
    }
    Ok((extension, metadata.len(), duration))
}

pub(crate) fn fast_transcription_endpoint(endpoint: &str) -> Result<reqwest::Url, String> {
    const AZURE_COGNITIVE_SERVICES_SUFFIX: &str = ".cognitiveservices.azure.com";

    let mut endpoint = reqwest::Url::parse(endpoint.trim()).map_err(|_| {
        "Use the Azure Speech custom domain endpoint, for example https://your-resource.cognitiveservices.azure.com/.".to_string()
    })?;
    let host = endpoint.host_str().unwrap_or_default();
    let resource_name =
        host.strip_suffix(AZURE_COGNITIVE_SERVICES_SUFFIX)
            .filter(|resource_name| {
                !resource_name.is_empty()
                    && resource_name
                        .chars()
                        .all(|character| character.is_ascii_alphanumeric() || character == '-')
            });
    if endpoint.scheme() != "https"
        || resource_name.is_none()
        || !endpoint.username().is_empty()
        || endpoint.password().is_some()
        || endpoint.port().is_some_and(|port| port != 443)
        || endpoint.path() != "/"
        || endpoint.query().is_some()
        || endpoint.fragment().is_some()
    {
        return Err("Use the Azure Speech custom domain endpoint, for example https://your-resource.cognitiveservices.azure.com/.".to_string());
    }
    endpoint.set_path("/speechtotext/transcriptions:transcribe");
    endpoint.set_query(Some("api-version=2025-10-15"));
    Ok(endpoint)
}

struct TemporaryMediaFile(Option<PathBuf>);

impl TemporaryMediaFile {
    fn new(path: PathBuf) -> Self {
        Self(Some(path))
    }

    fn keep(mut self) {
        self.0 = None;
    }
}

impl Drop for TemporaryMediaFile {
    fn drop(&mut self) {
        if let Some(path) = self.0.as_ref() {
            let _ = fs::remove_file(path);
        }
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

#[cfg(test)]
mod language_tests {
    use super::*;

    #[test]
    fn locale_normalization_roundtrips_canonical_azure_values() {
        for (input, expected) in [
            (" en-us ", "en-US"), ("KO_kr", "ko-KR"), ("ja-jp", "ja-JP"),
            ("ZH-cn", "zh-CN"), ("es-es", "es-ES"), ("fr-fr", "fr-FR"),
            ("DE-de", "de-DE"), ("en-gb", "en-GB"), ("sr-latn-rs", "sr-Latn-RS"),
        ] {
            assert_eq!(normalize_language(Some(input.into())), expected);
            assert_eq!(normalize_language(Some(expected.into())), expected);
        }
    }

    #[test]
    fn azure_bare_languages_use_supported_ui_locale_defaults() {
        for (input, expected) in [
            ("en", "en-US"), ("ko", "ko-KR"), ("ja", "ja-JP"), ("zh", "zh-CN"),
            ("es", "es-ES"), ("fr", "fr-FR"), ("de", "de-DE"), ("auto", "en-US"),
        ] {
            assert_eq!(normalize_azure_language(Some(input.into())), expected);
        }
        assert_eq!(normalize_language(Some("auto".into())), "auto");
        assert_eq!(normalize_language(Some("KO".into())), "ko");
        assert_eq!(normalize_language(Some("  ".into())), default_language());
        assert_eq!(normalize_language(None), default_language());
    }
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
    let value = language.unwrap_or_else(default_language);
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return default_language();
    }
    trimmed.replace('_', "-").split('-').enumerate().map(|(index, part)| {
        if index == 0 {
            part.to_ascii_lowercase()
        } else if part.len() == 2 && part.bytes().all(|byte| byte.is_ascii_alphabetic()) {
            part.to_ascii_uppercase()
        } else if part.len() == 4 && part.bytes().all(|byte| byte.is_ascii_alphabetic()) {
            let mut script = part.to_ascii_lowercase();
            script[..1].make_ascii_uppercase();
            script
        } else {
            part.to_ascii_lowercase()
        }
    }).collect::<Vec<_>>().join("-")
}

fn normalized_meeting(mut meeting: Meeting) -> Meeting {
    meeting.language = if meeting.transcription_engine.starts_with("azure") {
        normalize_azure_language(Some(meeting.language))
    } else {
        normalize_language(Some(meeting.language))
    };
    meeting
}

fn normalize_azure_language(language: Option<String>) -> String {
    let language = normalize_language(language);
    match language.as_str() {
        "auto" | "en" => "en-US".into(),
        "ko" => "ko-KR".into(),
        "ja" => "ja-JP".into(),
        "zh" => "zh-CN".into(),
        "es" => "es-ES".into(),
        "fr" => "fr-FR".into(),
        "de" => "de-DE".into(),
        _ => language,
    }
}

fn normalized_settings(mut settings: AppSettings) -> AppSettings {
    for language in [&mut settings.language, &mut settings.foundry_local_language] {
        // Blank settings are intentionally left blank for frontend defaults.
        if !language.trim().is_empty() {
            *language = normalize_language(Some(language.clone()));
        }
    }
    if !settings.azure_language.trim().is_empty() {
        settings.azure_language = normalize_azure_language(Some(settings.azure_language));
    }
    settings
}

fn normalize_capture_mode(capture_mode: Option<String>) -> String {
    match capture_mode.unwrap_or_else(default_capture_mode).as_str() {
        "microphone" => "microphone".to_string(),
        "system" => "system".to_string(),
        _ => default_capture_mode(),
    }
}

pub(crate) fn append_segment_to_store(
    app: &AppHandle,
    meeting_id: &str,
    segment: TranscriptSegment,
) -> Result<()> {
    let state = app.state::<AppState>();
    let mut store = state
        .store
        .lock()
        .map_err(|_| anyhow!("Store lock poisoned"))?;
    if let Some(meeting) = store.meetings.iter_mut().find(|item| item.id == meeting_id) {
        meeting.transcript.push(segment);
        meeting.updated_at = now_string();
    }
    write_store(&state.data_dir, &store)
}

#[tauri::command]
pub(crate) fn get_meetings(state: tauri::State<'_, AppState>) -> Result<Vec<Meeting>, String> {
    let store = state
        .store
        .lock()
        .map_err(|_| "Store lock poisoned".to_string())?;
    Ok(store.meetings.iter().cloned().map(normalized_meeting).collect())
}

#[tauri::command]
pub(crate) fn refresh_meetings(state: tauri::State<'_, AppState>) -> Result<Vec<Meeting>, String> {
    let refreshed = read_store(&state.data_dir).map_err(|error| error.to_string())?;
    let meetings: Vec<Meeting> = refreshed
        .meetings
        .into_iter()
        .map(normalized_meeting)
        .filter(|meeting| match &meeting.recording_path {
            // Drop meetings whose recorded audio file was deleted from disk.
            Some(path) => Path::new(path).exists(),
            // Keep transcript-only meetings that never had an audio file.
            None => true,
        })
        .collect();
    let mut store = state
        .store
        .lock()
        .map_err(|_| "Store lock poisoned".to_string())?;
    store.meetings = meetings.clone();
    write_store(&state.data_dir, &store).map_err(|error| error.to_string())?;
    Ok(meetings)
}

#[tauri::command]
pub(crate) fn get_settings(state: tauri::State<'_, AppState>) -> Result<AppSettings, String> {
    let store = state
        .store
        .lock()
        .map_err(|_| "Store lock poisoned".to_string())?;
    Ok(normalized_settings(store.settings.clone()))
}

#[tauri::command]
pub(crate) fn save_settings(
    state: tauri::State<'_, AppState>,
    settings: AppSettings,
) -> Result<(), String> {
    let mut store = state
        .store
        .lock()
        .map_err(|_| "Store lock poisoned".to_string())?;
    let previous = std::mem::replace(&mut store.settings, normalized_settings(settings));
    if let Err(error) = write_store(&state.data_dir, &store) {
        store.settings = previous;
        return Err(error.to_string());
    }
    Ok(())
}

#[tauri::command]
pub(crate) fn delete_meeting(
    state: tauri::State<'_, AppState>,
    meeting_id: String,
) -> Result<(), String> {
    // Keep the assistant lock through deletion: in-flight inference cannot
    // recreate private content after the meeting is removed.
    let _assistant_guard = crate::copilot::remove_for_meeting(&state.data_dir, &meeting_id)?;
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
        let speaker = segment.speaker_name.clone()
            .or_else(|| segment.speaker_id.as_ref().map(|id| format!("Speaker {id}")))
            .map(|name| format!("{name}: ")).unwrap_or_default();
        text.push_str(&format!(
            "[{}] {}{}\n",
            format_timestamp(segment.offset_seconds),
            speaker,
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

    let extension = Path::new(&source)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_else(|| "wav".to_string());
    let (filter_name, filter_extensions): (&str, &[&str]) = match extension.as_str() {
        "mp3" => ("MP3 audio", &["mp3"]),
        _ => ("WAV audio", &["wav"]),
    };
    let default_name = format!("{}-recording.{extension}", safe_file_stem(&meeting.title));
    let Some(path) = app
        .dialog()
        .file()
        .add_filter(filter_name, filter_extensions)
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
            LogicalSize::new(960.0, 560.0),
            LogicalSize::new(1040.0, 760.0),
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

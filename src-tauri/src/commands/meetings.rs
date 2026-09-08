use super::*;
use crate::audio::{build_recording_streams, PendingAudio, RecordingCapture};
use crate::foundry_local::{self, FoundryLocalModelCatalogEntry, FoundryLocalTranscriptionRequest};
use crate::speech_gate::{self, SpeechUtterance, StreamingSpeechSegmenter};
use crate::windows_call_mute::TeamsMuteMonitor;
use crate::TranscriptionEngine;
use std::io::Cursor;
use std::sync::atomic::AtomicBool;
use std::sync::OnceLock;
use std::time::Instant;

const FOUNDRY_VAD_TICK_MILLISECONDS: u64 = 20;
const FOUNDRY_INFERENCE_QUEUE_CAPACITY: usize = 4;
const FIXED_FOUNDRY_CHUNK_SECONDS: u64 = 5;
const BACKGROUND_TASK_STOP_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FoundryChunkingMode {
    Utterance,
    FixedFiveSeconds,
}

pub(crate) fn recover_incomplete_audio_encodings(app: &AppHandle, state: &AppState) -> Result<()> {
    let pending = state
        .store
        .lock()
        .map_err(|_| anyhow!("Store lock poisoned"))?
        .meetings
        .iter()
        .filter_map(|meeting| {
            let path = meeting.recording_path.as_deref()?;
            let path = PathBuf::from(path);
            (meeting.transcription_engine != "azure-fast"
                && path
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("wav"))
                && path.is_file())
            .then(|| (meeting.id.clone(), path))
        })
        .collect::<Vec<_>>();

    for (meeting_id, wav_path) in pending {
        let mp3_path = wav_path.with_extension("mp3");
        if let Err(error) = video::encode_mp3(app, &wav_path, &mp3_path) {
            eprintln!("Could not recover audio encoding for {meeting_id}: {error}");
            continue;
        }

        let mut store = state
            .store
            .lock()
            .map_err(|_| anyhow!("Store lock poisoned"))?;
        let Some(meeting) = store.meetings.iter_mut().find(|meeting| {
            meeting.id == meeting_id
                && meeting.recording_path.as_deref() == Some(wav_path.to_string_lossy().as_ref())
        }) else {
            continue;
        };
        meeting.recording_path = Some(mp3_path.to_string_lossy().to_string());
        meeting.updated_at = now_string();
        write_store(&state.data_dir, &store)?;
        drop(store);
        let _ = fs::remove_file(wav_path);
    }

    Ok(())
}

fn wav_bytes(samples: &[f32], sample_rate: u32) -> Result<Vec<u8>> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut cursor = Cursor::new(Vec::new());
    {
        let mut writer = hound::WavWriter::new(&mut cursor, spec)?;
        for sample in samples {
            writer.write_sample(crate::audio::pcm16_sample(*sample))?;
        }
        writer.finalize()?;
    }
    Ok(cursor.into_inner())
}

fn take_pending_audio(pending: &Arc<Mutex<PendingAudio>>) -> Result<Vec<f32>, String> {
    pending.lock().map_err(|_| "Pending audio buffer lock poisoned".to_string())?.take()
}

#[derive(Debug)]
enum RecordingFailure {
    Capture(String),
    Persistence(String),
}

fn flush_recording_samples(
    samples: &Arc<Mutex<RecordingCapture>>,
    writer: &Arc<Mutex<Option<crate::audio::IncrementalWavWriter>>>,
    pending: &Arc<Mutex<PendingAudio>>,
    final_chunk: bool,
) -> Result<(), RecordingFailure> {
    loop {
        let mixed = samples
            .lock()
            .map_err(|_| RecordingFailure::Capture("Recording sample buffer lock poisoned".into()))?
            .drain(final_chunk)
            .map_err(RecordingFailure::Capture)?;
        if mixed.is_empty() {
            return Ok(());
        }
        let mut writer = writer
            .lock()
            .map_err(|_| RecordingFailure::Persistence("Audio writer lock poisoned".into()))?;
        let writer = writer.as_mut().ok_or_else(|| RecordingFailure::Persistence("Audio writer already closed".into()))?;
        append_wav_chunk(writer, &mixed).map_err(|error| RecordingFailure::Persistence(error.to_string()))?;
        // Only publish samples successfully persisted, without re-mixing them.
        pending.lock().map_err(|_| RecordingFailure::Persistence("Pending audio buffer lock poisoned".into()))?.push(&mixed);
    }
}

/// Stop native capture even if the webview is absent. Overflow/stream errors
/// leave accepted packets intact, so save those before closing the WAV. A disk
/// write failure may have partially written a chunk: never retry/append after it.
fn finish_failed_recording(
    failure: RecordingFailure,
    samples: &Arc<Mutex<RecordingCapture>>,
    writer: &Arc<Mutex<Option<crate::audio::IncrementalWavWriter>>>,
    pending: &Arc<Mutex<PendingAudio>>,
    audio_stop: &std::sync::mpsc::Sender<()>,
) -> String {
    let _ = audio_stop.send(());
    let recover_queued = matches!(&failure, RecordingFailure::Capture(_));
    let mut message = match failure {
        RecordingFailure::Capture(message) | RecordingFailure::Persistence(message) => message,
    };
    if recover_queued {
        if let Err(error) = flush_recording_samples(samples, writer, pending, true) {
            message.push_str(&format!(" Queued audio recovery failed: {error:?}."));
        }
    }
    match writer.lock() {
        Ok(mut writer) => {
            if let Some(writer) = writer.take() {
                if let Err(error) = writer.finalize() {
                    message.push_str(&format!(" WAV finalization failed: {error}."));
                }
            }
        }
        Err(_) => message.push_str(" Audio writer lock poisoned; WAV finalization could not be confirmed."),
    }
    message
}

// Frontend contract: listen before starting capture; queue events received
// during start_recording. For the matching session, display message and invoke
// the normal stop flow once (including Azure shutdown), then refresh meetings
// even if stop_recording rejects. recordingPath identifies the retained NEW WAV;
// on append, it is deliberately not a replacement for the previous recording.
fn recording_error_payload(meeting_id: &str, message: &str, recording_path: &Path) -> serde_json::Value {
    serde_json::json!({
        "meetingId": meeting_id,
        "message": message,
        "fatal": true,
        "recordingPath": recording_path.to_string_lossy(),
    })
}

fn foundry_request_for_job(
    request: &FoundryLocalTranscriptionRequest,
    language: &Mutex<String>,
) -> Result<FoundryLocalTranscriptionRequest, String> {
    let language = language.lock().map_err(|_| "Recording language lock poisoned".to_string())?;
    let locale = normalize_language(Some(language.clone()));
    let mut request = request.clone();
    request.language = if locale == "auto" { None }
        else { Some(locale.split('-').next().unwrap_or("en").to_string()) };
    Ok(request)
}

fn captured_wav_duration(path: &Path) -> Result<f64, String> {
    let reader = hound::WavReader::open(path).map_err(|error| format!("Could not read captured WAV duration: {error}"))?;
    let rate = reader.spec().sample_rate;
    if rate == 0 { return Err("Captured WAV has an invalid sample rate".into()); }
    Ok(reader.duration() as f64 / f64::from(rate))
}

/// MP3 metadata can be an estimate (especially VBR). Count decoded interleaved
/// samples with bounded memory, using the same decoder as native playback.
fn previous_audio_duration(path: &Path) -> Result<f64, String> {
    if path.extension().is_some_and(|extension| extension.eq_ignore_ascii_case("wav")) {
        return captured_wav_duration(path);
    }
    let file = fs::File::open(path).map_err(|error| format!("Could not read previous audio: {error}"))?;
    let mut decoder = Decoder::new(BufReader::new(file))
        .map_err(|error| format!("Could not determine exact previous audio duration: {error}"))?;
    let rate = decoder.sample_rate();
    let channels = decoder.channels();
    if rate == 0 || channels == 0 { return Err("Previous audio has invalid sample metadata".into()); }
    let mut samples = 0_u64;
    while decoder.next().is_some() {
        if decoder.sample_rate() != rate || decoder.channels() != channels {
            return Err("Cannot determine exact append duration for changing audio formats".into());
        }
        samples += 1;
    }
    if samples == 0 || samples % u64::from(channels) != 0 {
        return Err("Cannot determine exact append duration from the previous audio".into());
    }
    Ok(samples as f64 / f64::from(channels) / f64::from(rate))
}

async fn await_task_completion(
    receiver: tokio::sync::oneshot::Receiver<Result<(), String>>,
    task_name: &str,
    timeout: Duration,
) -> Result<(), String> {
    match tokio::time::timeout(timeout, receiver).await {
        Ok(Ok(result)) => result,
        Ok(Err(_)) => Err(format!("{task_name} stopped without reporting completion")),
        Err(_) => Err(format!(
            "{task_name} did not stop within {} seconds",
            timeout.as_secs()
        )),
    }
}

struct FoundryUtteranceJob {
    utterance: SpeechUtterance,
    request: FoundryLocalTranscriptionRequest,
}

async fn enqueue_utterances(
    sender: &mpsc::Sender<FoundryUtteranceJob>,
    utterances: Vec<SpeechUtterance>,
    request: &FoundryLocalTranscriptionRequest,
    language: &Mutex<String>,
) -> Result<(), String> {
    if utterances.is_empty() {
        return Ok(());
    }
    // Bind the language when segmentation emits the utterance, before queue
    // backpressure or inference delays can let a later setting change it.
    let request = foundry_request_for_job(request, language)?;
    for utterance in utterances {
        sender
            .send(FoundryUtteranceJob { utterance, request: request.clone() })
            .await
            .map_err(|_| "Foundry Local inference worker stopped unexpectedly".to_string())?;
    }
    Ok(())
}

async fn run_foundry_inference_worker(
    mut receiver: mpsc::Receiver<FoundryUtteranceJob>,
    app: AppHandle,
    meeting_id: String,
    base_offset_seconds: f64,
) -> Result<(), String> {
    while let Some(FoundryUtteranceJob { utterance, request }) = receiver.recv().await {
        let offset_seconds =
            base_offset_seconds + utterance.offset_samples as f64 / TRANSCRIPT_SAMPLE_RATE;
        let result = tokio::task::spawn_blocking(move || {
            let bytes = wav_bytes(&utterance.samples, TRANSCRIPT_SAMPLE_RATE as u32)?;
            foundry_local::transcribe_foundry_local_audio(request, bytes)
                .map(|transcription| transcription.accepted_text())
                .map_err(|error| anyhow!(error.to_string()))
        })
        .await
        .unwrap_or_else(|error| Err(anyhow!("Foundry Local task panicked: {error}")));

        match result {
            Ok(Some(text)) => {
                let segment = TranscriptSegment {
                    id: format!("segment-{}", Uuid::new_v4()),
                    offset_seconds,
                    text,
                    speaker_id: None,
                    speaker_name: None,
                };
                if let Err(error) = append_segment_to_store(&app, &meeting_id, segment.clone()) {
                    let message = error.to_string();
                    let _ = app.emit("transcription-error", message.clone());
                    continue;
                }
                let _ = app.emit(
                    "transcript-segment",
                    TranscriptEvent {
                        meeting_id: meeting_id.clone(),
                        segment,
                    },
                );
            }
            Ok(None) => {}
            Err(error) => {
                let message = error.to_string();
                let _ = app.emit("transcription-error", message.clone());
            }
        }
    }

    Ok(())
}

async fn run_foundry_transcription_task(
    pending: Arc<Mutex<PendingAudio>>,
    mut stop_rx: mpsc::Receiver<()>,
    request: FoundryLocalTranscriptionRequest,
    language: Arc<Mutex<String>>,
    app: AppHandle,
    meeting_id: String,
    base_offset_seconds: f64,
) -> Result<(), String> {
    let mut segmenter = StreamingSpeechSegmenter::new()?;
    let (utterance_tx, utterance_rx) = mpsc::channel(FOUNDRY_INFERENCE_QUEUE_CAPACITY);
    let worker = tauri::async_runtime::spawn(run_foundry_inference_worker(
        utterance_rx,
        app.clone(),
        meeting_id,
        base_offset_seconds,
    ));
    let mut interval = tokio::time::interval(Duration::from_millis(FOUNDRY_VAD_TICK_MILLISECONDS));
    let coordinator_result: Result<(), String> = async {
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    let samples = take_pending_audio(&pending)?;
                    if samples.is_empty() {
                        continue;
                    }

                    let utterances = segmenter.push_samples(&samples)?;
                    enqueue_utterances(&utterance_tx, utterances, &request, &language).await?;
                }
                _ = stop_rx.recv() => {
                    let final_samples = take_pending_audio(&pending)?;
                    if !final_samples.is_empty() {
                        let utterances = segmenter.push_samples(&final_samples)?;
                        enqueue_utterances(&utterance_tx, utterances, &request, &language).await?;
                    }
                    enqueue_utterances(&utterance_tx, segmenter.flush()?, &request, &language).await?;
                    break Ok(());
                }
            }
        }
    }
    .await;
    drop(utterance_tx);
    let worker_result = worker
        .await
        .map_err(|error| format!("Foundry Local inference worker panicked: {error}"))?;
    coordinator_result.and(worker_result)
}

async fn run_fixed_foundry_transcription_task(
    pending: Arc<Mutex<PendingAudio>>,
    mut stop_rx: mpsc::Receiver<()>,
    request: FoundryLocalTranscriptionRequest,
    language: Arc<Mutex<String>>,
    app: AppHandle,
    meeting_id: String,
    base_offset_seconds: f64,
) -> Result<(), String> {
    let (utterance_tx, utterance_rx) = mpsc::channel(FOUNDRY_INFERENCE_QUEUE_CAPACITY);
    let worker = tauri::async_runtime::spawn(run_foundry_inference_worker(
        utterance_rx,
        app,
        meeting_id,
        base_offset_seconds,
    ));
    let mut interval = tokio::time::interval(Duration::from_secs(FIXED_FOUNDRY_CHUNK_SECONDS));
    interval.tick().await;
    let mut offset_samples = 0_u64;
    let coordinator_result: Result<(), String> = async {
        loop {
            let samples = tokio::select! {
                _ = interval.tick() => take_pending_audio(&pending)?,
                _ = stop_rx.recv() => {
                    let final_samples = take_pending_audio(&pending)?;
                    if !final_samples.is_empty()
                        && speech_gate::has_sufficient_speech(&final_samples)?
                    {
                        enqueue_utterances(&utterance_tx, vec![SpeechUtterance {
                            offset_samples,
                            samples: final_samples,
                            voiced_frames: usize::MAX,
                        }], &request, &language).await?;
                    }
                    break Ok(());
                }
            };
            if samples.is_empty() {
                continue;
            }
            let chunk_offset = offset_samples;
            offset_samples += samples.len() as u64;
            if speech_gate::has_sufficient_speech(&samples)? {
                enqueue_utterances(
                    &utterance_tx,
                    vec![SpeechUtterance {
                        offset_samples: chunk_offset,
                        samples,
                        voiced_frames: usize::MAX,
                    }],
                    &request,
                    &language,
                )
                .await?;
            }
        }
    }
    .await;
    drop(utterance_tx);
    let worker_result = worker
        .await
        .map_err(|error| format!("Foundry Local inference worker panicked: {error}"))?;
    coordinator_result.and(worker_result)
}

async fn run_azure_transcription_task(
    pending: Arc<Mutex<PendingAudio>>,
    mut stop_rx: mpsc::Receiver<()>,
    app: AppHandle,
    meeting_id: String,
    base_offset_seconds: f64,
) -> Result<(), String> {
    let mut interval = tokio::time::interval(Duration::from_millis(250));
    let mut emitted_samples = 0_u64;
    loop {
        tokio::select! {
            _ = interval.tick() => {
                let samples = take_pending_audio(&pending)?;
                if samples.is_empty() {
                    continue;
                }
                let segment_offset = base_offset_seconds + emitted_samples as f64 / TRANSCRIPT_SAMPLE_RATE;
                emitted_samples += samples.len() as u64;
                let _ = app.emit("audio-chunk", AudioChunkEvent {
                    meeting_id: meeting_id.clone(),
                    offset_seconds: segment_offset,
                    pcm_base64: pcm16_base64(&samples),
                });
            }
            _ = stop_rx.recv() => {
                let samples = take_pending_audio(&pending)?;
                if !samples.is_empty() {
                    let _ = app.emit("audio-chunk", AudioChunkEvent {
                        meeting_id,
                        offset_seconds: base_offset_seconds + emitted_samples as f64 / TRANSCRIPT_SAMPLE_RATE,
                        pcm_base64: pcm16_base64(&samples),
                    });
                }
                return Ok(());
            },
        }
    }
}

fn normalize_transcription_engine(value: Option<String>) -> TranscriptionEngine {
    match value
        .as_deref()
        .map(str::trim)
        .map(str::to_lowercase)
        .as_deref()
    {
        Some("azure") => TranscriptionEngine::Azure,
        Some("foundrylocal") | Some("foundry-local") | Some("foundry_local") => {
            TranscriptionEngine::FoundryLocal
        }
        _ => TranscriptionEngine::FoundryLocal,
    }
}

fn normalize_foundry_chunking_mode(value: Option<String>) -> FoundryChunkingMode {
    match value
        .as_deref()
        .map(str::trim)
        .map(str::to_lowercase)
        .as_deref()
    {
        Some("fixed5seconds") | Some("fixed-five-seconds") | Some("fixed") => {
            FoundryChunkingMode::FixedFiveSeconds
        }
        _ => FoundryChunkingMode::Utterance,
    }
}

fn foundry_download_lock() -> &'static std::sync::Mutex<()> {
    static LOCK: OnceLock<std::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
}

#[tauri::command]
pub(crate) fn list_foundry_local_models() -> Result<Vec<FoundryLocalModelCatalogEntry>, String> {
    foundry_local::list_foundry_local_models().map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) async fn download_foundry_local_model(
    app: AppHandle,
    alias: String,
) -> Result<(), String> {
    let alias = alias.trim().to_string();
    if alias.is_empty() {
        return Err("Foundry Local model alias is required.".to_string());
    }

    let progress_app = app.clone();
    let progress_alias = alias.clone();
    let progress_state = Arc::new(Mutex::new((
        String::new(),
        -1.0_f64,
        Instant::now() - Duration::from_secs(1),
    )));
    let on_progress: foundry_local::ProgressCallback = Arc::new(move |phase, percent| {
        let now = Instant::now();
        let mut should_emit = percent >= 100.0;
        if let Ok(mut last) = progress_state.lock() {
            should_emit = should_emit
                || last.0 != phase
                || (percent - last.1).abs() >= 1.0
                || now.duration_since(last.2) >= Duration::from_millis(250);
            if should_emit {
                *last = (phase.to_string(), percent, now);
            }
        }
        if should_emit {
            let _ = progress_app.emit(
                "foundry-download-progress",
                serde_json::json!({
                    "alias": progress_alias,
                    "phase": phase,
                    "percent": percent,
                }),
            );
        }
    });

    let task_alias = alias.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let _guard = foundry_download_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        foundry_local::prepare_foundry_local_model(&task_alias, on_progress)
    })
    .await
    .map_err(|error| format!("Foundry Local model download task failed: {error}"))?;

    outcome.map_err(|error| error.to_string())?;
    let _ = app.emit(
        "foundry-download-progress",
        serde_json::json!({
            "alias": alias,
            "phase": "done",
            "percent": 100.0,
        }),
    );
    Ok(())
}

#[tauri::command]
pub(crate) fn list_audio_input_devices() -> Result<Vec<AudioInputDevice>, String> {
    list_input_devices()
}

#[tauri::command]
pub(crate) fn list_audio_output_devices() -> Result<Vec<AudioOutputDevice>, String> {
    list_output_devices()
}

// The flat argument list preserves the existing Tauri IPC payload.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub(crate) fn start_recording(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    meeting_title: Option<String>,
    append_to_meeting_id: Option<String>,
    audio_device_id: Option<String>,
    system_audio_device_id: Option<String>,
    capture_mode: Option<String>,
    language: Option<String>,
    transcription_engine: Option<String>,
    foundry_local_model_alias: Option<String>,
    foundry_local_chunking_mode: Option<String>,
) -> Result<Meeting, String> {
    let _lifecycle_guard = state
        .recording_lifecycle_lock
        .lock()
        .map_err(|_| "Recording lifecycle lock poisoned".to_string())?;
    if state
        .recorder
        .lock()
        .map_err(|_| "Recorder lock poisoned".to_string())?
        .is_some()
        || state
            .screen_recorder
            .lock()
            .map_err(|_| "Screen recorder lock poisoned".to_string())?
            .is_some()
    {
        return Err("Stop the current recording before starting another one.".to_string());
    }

    let transcription_engine = normalize_transcription_engine(transcription_engine);
    let foundry_chunking_mode = normalize_foundry_chunking_mode(foundry_local_chunking_mode);
    // Resolve append state and duration before starting any capture threads.
    let append_to_meeting_id = append_to_meeting_id
        .map(|id| id.trim().to_string()).filter(|id| !id.is_empty());
    let existing_meeting = if let Some(meeting_id) = append_to_meeting_id.as_deref() {
        let store = state.store.lock().map_err(|_| "Store lock poisoned".to_string())?;
        Some(store.meetings.iter().find(|meeting| meeting.id == meeting_id).cloned()
            .ok_or_else(|| "The open meeting session no longer exists.".to_string())?)
    } else { None };
    let prior_recording_path = existing_meeting.as_ref()
        .and_then(|meeting| meeting.recording_path.as_deref()).map(PathBuf::from);
    let initial_transcript_offset = match prior_recording_path.as_deref() {
        Some(path) => previous_audio_duration(path)?,
        None if existing_meeting.as_ref().is_some_and(|meeting| !meeting.transcript.is_empty()) => {
            return Err("Cannot append with an exact audio timeline: the prior transcript has no saved audio.".into());
        }
        None => 0.0,
    };
    let requested_language = existing_meeting.as_ref()
        .map(|meeting| meeting.language.clone()).or(language);
    let language = if transcription_engine == TranscriptionEngine::Azure {
        normalize_azure_language(requested_language)
    } else {
        normalize_language(requested_language)
    };
    let foundry_request = if transcription_engine == TranscriptionEngine::FoundryLocal {
        let model_alias = foundry_local_model_alias
            .unwrap_or_default()
            .trim()
            .to_string();
        if model_alias.is_empty() {
            return Err("Select a Foundry Local speech model before recording.".to_string());
        }
        foundry_local::ensure_foundry_local_model_ready(&model_alias)
            .map_err(|error| error.to_string())?;
        Some(FoundryLocalTranscriptionRequest {
            model_alias,
            language: Some(language.clone()),
        })
    } else {
        None
    };

    let capture_mode = normalize_capture_mode(capture_mode);
    let capture_microphone = capture_mode != "system";
    let capture_system = capture_mode != "microphone";
    let recording_samples = Arc::new(Mutex::new(RecordingCapture::new(capture_microphone, capture_system)));
    let pending = Arc::new(Mutex::new(PendingAudio::default()));
    let microphone_muted = Arc::new(AtomicBool::new(false));
    let recording_path = state
        .data_dir
        .join("recordings")
        .join(format!("{}.wav", Uuid::new_v4()));
    let audio_writer = Arc::new(Mutex::new(Some(
        create_incremental_wav(&recording_path, 16_000).map_err(|error| error.to_string())?,
    )));

    // `cpal::Stream` is `!Send`, so a dedicated thread owns the streams for the
    // whole recording. `start_recording` only keeps `Send` control handles,
    // which makes "a stream crossing threads" unrepresentable and removes the
    // need for any `unsafe impl Send`.
    let (setup_tx, setup_rx) =
        std::sync::mpsc::channel::<Result<crate::audio::AudioSetup, String>>();
    let (audio_stop_tx, audio_stop_rx) = std::sync::mpsc::channel::<()>();
    let (audio_done_tx, audio_done_rx) = std::sync::mpsc::channel::<()>();
    let recording_samples_for_audio = recording_samples.clone();
    let microphone_muted_for_audio = microphone_muted.clone();
    let audio_thread = std::thread::spawn(move || {
        match build_recording_streams(
            capture_microphone,
            capture_system,
            audio_device_id.as_deref(),
            system_audio_device_id.as_deref(),
            &capture_mode,
            recording_samples_for_audio,
            microphone_muted_for_audio,
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
        let _ = audio_done_tx.send(());
    });

    let setup = match setup_rx.recv_timeout(AUDIO_DEVICE_SETUP_TIMEOUT) {
        Ok(Ok(setup)) => setup,
        Ok(Err(error)) => return Err(error),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            return Err(format!(
                "Audio devices did not initialize within {} seconds.",
                AUDIO_DEVICE_SETUP_TIMEOUT.as_secs()
            ));
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            return Err("Audio capture thread stopped unexpectedly".to_string());
        }
    };

    let engine = match transcription_engine {
        TranscriptionEngine::Azure => "azure".to_string(),
        TranscriptionEngine::FoundryLocal => "foundryLocal".to_string(),
    };
    let is_appending = existing_meeting.is_some();
    // When appending, keep the open session's transcript and existing audio and
    // just refresh the capture metadata for the new segment.
    let mut meeting = match existing_meeting {
        Some(mut meeting) => {
            meeting.duration_seconds = initial_transcript_offset;
            meeting.audio_device_id = setup.resolved_audio_device_id;
            meeting.audio_device_name = setup.audio_device_name;
            meeting.system_audio_device_id = setup.resolved_system_audio_device_id;
            meeting.system_audio_device_name = setup.system_audio_device_name;
            meeting.capture_mode = setup.resolved_capture_mode;
            meeting.language = language.clone();
            meeting.transcription_engine = engine;
            meeting
        }
        None => make_meeting(MeetingOptions {
            title: meeting_title,
            audio_device_id: setup.resolved_audio_device_id,
            audio_device_name: setup.audio_device_name,
            system_audio_device_id: setup.resolved_system_audio_device_id,
            system_audio_device_name: setup.system_audio_device_name,
            capture_mode: setup.resolved_capture_mode,
            language: language.clone(),
            transcription_engine: engine,
        }),
    };
    // Persist the destination before capture begins. Each chunk is flushed by
    // the recording task, so a crash leaves the completed portion on disk.
    meeting.has_audio = true;
    if !is_appending {
        meeting.recording_path = Some(recording_path.to_string_lossy().to_string());
    }

    let persisted = {
        let mut store = state
            .store
            .lock()
            .map_err(|_| "Store lock poisoned".to_string())?;
        if is_appending {
            let target = store
                .meetings
                .iter_mut()
                .find(|item| item.id == meeting.id)
                .ok_or_else(|| "The open meeting session no longer exists.".to_string())?;
            *target = meeting.clone();
        } else {
            store.meetings.insert(0, meeting.clone());
        }
        let persist = write_store(&state.data_dir, &store).map_err(|error| error.to_string());
        if persist.is_err() {
            if !is_appending {
                store.meetings.retain(|item| item.id != meeting.id);
            }
        }
        persist
    };
    if let Err(error) = persisted {
        let _ = audio_stop_tx.send(());
        if audio_done_rx
            .recv_timeout(AUDIO_THREAD_STOP_TIMEOUT)
            .is_ok()
        {
            let _ = audio_thread.join();
        }
        let _ = fs::remove_file(&recording_path);
        return Err(error);
    }

    let teams_mute_monitor = if capture_microphone {
        match TeamsMuteMonitor::start(app.clone(), meeting.id.clone(), microphone_muted.clone()) {
            Ok(session) => Some(session),
            Err(message) => {
                let _ = app.emit(
                    "call-mute-warning",
                    serde_json::json!({
                        "meetingId": meeting.id,
                        "message": message,
                    }),
                );
                None
            }
        }
    } else {
        None
    };

    let (recording_stop_tx, mut recording_stop_rx) = mpsc::channel::<()>(1);
    let (recording_done_tx, recording_done_rx) =
        tokio::sync::oneshot::channel::<Result<(), String>>();
    let recording_samples_for_task = recording_samples.clone();
    let audio_writer_for_task = audio_writer.clone();
    let pending_for_recording = pending.clone();
    let app_for_recording = app.clone();
    let audio_stop_for_recording = audio_stop_tx.clone();
    let meeting_id_for_recording = meeting.id.clone();
    let path_for_recording = recording_path.clone();
    let (stop_tx, stop_rx) = mpsc::channel::<()>(1);
    let transcription_stop_for_recording = stop_tx.clone();
    tauri::async_runtime::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(FOUNDRY_VAD_TICK_MILLISECONDS));
        let result = loop {
            tokio::select! {
                _ = interval.tick() => {
                    if let Err(error) =
                        flush_recording_samples(&recording_samples_for_task, &audio_writer_for_task, &pending_for_recording, false)
                    {
                        break Err(error);
                    }
                }
                _ = recording_stop_rx.recv() => {
                    let result = flush_recording_samples(
                        &recording_samples_for_task,
                        &audio_writer_for_task,
                        &pending_for_recording,
                        true,
                    );
                    // Stop can win the select before the next persistence tick.
                    // Saving the accepted tail must not hide a latched failure.
                    break result.and_then(|()| {
                        let capture = recording_samples_for_task.lock()
                            .map_err(|_| RecordingFailure::Capture("Recording sample buffer lock poisoned".into()))?;
                        match capture.error() {
                            Some(error) => Err(RecordingFailure::Capture(error.to_string())),
                            None => Ok(()),
                        }
                    });
                }
            }
        };
        match result {
            Ok(()) => { let _ = recording_done_tx.send(Ok(())); }
            Err(failure) => {
                let message = finish_failed_recording(
                    failure, &recording_samples_for_task, &audio_writer_for_task,
                    &pending_for_recording, &audio_stop_for_recording,
                );
                // Recovery publishes the accepted tail before transcription stops.
                let _ = transcription_stop_for_recording.try_send(());
                let message = format!("Recording stopped: {message} Partial WAV retained at {} (it may be incomplete if writing/finalization failed).", path_for_recording.display());
                let _ = recording_done_tx.send(Err(message.clone()));
                let _ = app_for_recording.emit("recording-error", recording_error_payload(
                    &meeting_id_for_recording, &message, &path_for_recording,
                ));
            }
        }
    });

    let (transcription_done_tx, transcription_done_rx) =
        tokio::sync::oneshot::channel::<Result<(), String>>();
    let meeting_id = meeting.id.clone();
    let app_for_task = app.clone();
    let pending_for_task = pending.clone();
    let language_for_task = Arc::new(Mutex::new(language));
    let language_for_recorder = language_for_task.clone();

    tauri::async_runtime::spawn(async move {
        let error_app = app_for_task.clone();
        let result = if let Some(request) = foundry_request {
            match foundry_chunking_mode {
                FoundryChunkingMode::Utterance => {
                    run_foundry_transcription_task(
                        pending_for_task,
                        stop_rx,
                        request,
                        language_for_task,
                        app_for_task,
                        meeting_id,
                        initial_transcript_offset,
                    )
                    .await
                }
                FoundryChunkingMode::FixedFiveSeconds => {
                    run_fixed_foundry_transcription_task(
                        pending_for_task,
                        stop_rx,
                        request,
                        language_for_task,
                        app_for_task,
                        meeting_id,
                        initial_transcript_offset,
                    )
                    .await
                }
            }
        } else {
            run_azure_transcription_task(
                pending_for_task,
                stop_rx,
                app_for_task,
                meeting_id,
                initial_transcript_offset,
            )
            .await
        };
        if let Err(error) = &result {
            let _ = error_app.emit("transcription-error", error.clone());
        }
        let _ = transcription_done_tx.send(result);
    });

    *state
        .recorder
        .lock()
        .map_err(|_| "Recorder lock poisoned".to_string())? = Some(RecorderSession {
        meeting_id: meeting.id.clone(),
        prior_recording_path,
        base_offset_seconds: initial_transcript_offset,
        stop_tx,
        transcription_done_rx,
        recording_stop_tx,
        recording_done_rx,
        audio_stop_tx,
        audio_thread,
        audio_done_rx,
        audio_writer,
        recording_path,
        language: language_for_recorder,
        microphone_muted,
        teams_mute_monitor,
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
        speaker_id: None,
        speaker_name: None,
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
pub(crate) async fn stop_recording(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<Meeting, String> {
    let session = {
        let _lifecycle_guard = state
            .recording_lifecycle_lock
            .lock()
            .map_err(|_| "Recording lifecycle lock poisoned".to_string())?;
        state
            .recorder
            .lock()
            .map_err(|_| "Recorder lock poisoned".to_string())?
            .take()
            .ok_or_else(|| "Recording is not running".to_string())?
    };

    let RecorderSession {
        meeting_id,
        prior_recording_path,
        base_offset_seconds,
        stop_tx,
        transcription_done_rx,
        recording_stop_tx,
        recording_done_rx,
        audio_stop_tx,
        audio_thread,
        audio_done_rx,
        audio_writer,
        recording_path: raw_recording_path,
        language: _,
        microphone_muted: _,
        teams_mute_monitor,
        started_at: _,
    } = session;

    let _ = audio_stop_tx.send(());
    let audio_stopped = tauri::async_runtime::spawn_blocking(move || {
        if audio_done_rx
            .recv_timeout(AUDIO_THREAD_STOP_TIMEOUT)
            .is_err()
        {
            return false;
        }
        let _ = audio_thread.join();
        true
    })
    .await
    .map_err(|error| format!("Could not wait for the audio capture thread: {error}"))?;
    if !audio_stopped {
        eprintln!(
            "Audio capture thread did not stop within {} seconds; continuing finalization.",
            AUDIO_THREAD_STOP_TIMEOUT.as_secs()
        );
    }
    drop(teams_mute_monitor);
    let _ = recording_stop_tx.send(()).await;
    // Final persistence publishes the tail before transcription is allowed to
    // drain and stop. Sending both stop signals together loses the last chunk.
    let recording_result = await_task_completion(
        recording_done_rx, "Audio persistence task", BACKGROUND_TASK_STOP_TIMEOUT,
    ).await;
    // A fatal persistence error may already have queued this stop. Do not wait
    // on a full control channel while an inference worker is backpressured.
    let _ = stop_tx.try_send(());

    {
        let mut writer = audio_writer
            .lock()
            .map_err(|_| "Audio writer lock poisoned".to_string())?;
        if let Some(writer) = writer.take() {
            writer.finalize().map_err(|error| error.to_string())?;
        }
    }
    let duration = captured_wav_duration(&raw_recording_path)
        .map_err(|error| format!("{error}. Retained WAV: {}", raw_recording_path.display()))?;
    let transcription_result = await_task_completion(
        transcription_done_rx, "Transcription task", BACKGROUND_TASK_STOP_TIMEOUT,
    ).await;
    if let Err(error) = recording_result {
        // Keep a failed new session playable with its exact saved duration. An
        // append failure must never repoint/overwrite the previous recording;
        // its separate recovery WAV is disclosed in recording-error instead.
        if prior_recording_path.is_none() {
            let mut store = state.store.lock().map_err(|_| "Store lock poisoned".to_string())?;
            if let Some(meeting) = store.meetings.iter_mut().find(|meeting| meeting.id == meeting_id) {
                meeting.duration_seconds = duration;
                meeting.recording_path = Some(raw_recording_path.to_string_lossy().to_string());
                meeting.has_audio = true;
                meeting.updated_at = now_string();
            }
            write_store(&state.data_dir, &store).map_err(|error| error.to_string())?;
        }
        return Err(error);
    }
    if !audio_stopped {
        return Err("Audio capture did not stop cleanly; retained the captured WAV. Exact final duration could not be confirmed.".into());
    }
    let encoded_path = raw_recording_path.with_extension("mp3");
    video::encode_mp3(&app, &raw_recording_path, &encoded_path)
        .map_err(|error| format!("Could not save the meeting audio as MP3: {error}"))?;
    // When appending, concatenate the prior meeting audio with the new segment
    // into a fresh file; otherwise the encoded MP3 is the final recording.
    let recording_path = match prior_recording_path.as_deref() {
        Some(previous_path) if previous_path.is_file() => {
            let merged_path = state.data_dir.join("recordings").join(format!(
                "{}-{}.mp3",
                meeting_id,
                Uuid::new_v4()
            ));
            video::concatenate_audio(&app, previous_path, &encoded_path, &merged_path)
                .map_err(|error| format!("Could not append the meeting audio: {error}"))?;
            merged_path
        }
        Some(_) => return Err("Previous audio disappeared while recording; retained the new WAV and MP3 rather than replacing the original meeting.".into()),
        None => encoded_path.clone(),
    };
    let persisted_meeting = read_store(&state.data_dir).ok().and_then(|store| {
        store
            .meetings
            .into_iter()
            .find(|meeting| meeting.id == meeting_id)
    });

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
        if let Some(persisted_meeting) = persisted_meeting {
            if persisted_meeting.transcript.len() > meeting.transcript.len() {
                meeting.transcript = persisted_meeting.transcript;
            }
        }
        meeting.duration_seconds = base_offset_seconds + duration;
        meeting.has_audio = true;
        meeting.recording_path = Some(recording_path.to_string_lossy().to_string());
        meeting.updated_at = now_string();
        meeting.clone()
    };
    state.save_store().map_err(|error| error.to_string())?;
    // Remove the raw WAV and any intermediate files the final MP3 superseded.
    let _ = fs::remove_file(&raw_recording_path);
    if encoded_path != recording_path {
        let _ = fs::remove_file(&encoded_path);
    }
    // Keep the original audio when appending; the merged file is a new result.
    transcription_result?;
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
    // Serialize with recording startup so it cannot publish a session carrying
    // the old language after this update succeeds. Only this meeting is bound.
    let _lifecycle_guard = state.recording_lifecycle_lock.lock()
        .map_err(|_| "Recording lifecycle lock poisoned".to_string())?;
    let recorder = state.recorder.lock().map_err(|_| "Recorder lock poisoned".to_string())?;
    let mut active_language = recorder.as_ref()
        .filter(|session| session.meeting_id == meeting_id)
        .map(|session| session.language.lock()
            .map_err(|_| "Recording language lock poisoned".to_string()))
        .transpose()?;
    let mut store = state.store.lock().map_err(|_| "Store lock poisoned".to_string())?;
    let index = store.meetings.iter().position(|meeting| meeting.id == meeting_id)
        .ok_or_else(|| "Meeting not found".to_string())?;
    let previous = store.meetings[index].clone();
    let language = if previous.transcription_engine.starts_with("azure") {
        normalize_azure_language(Some(language))
    } else {
        normalize_language(Some(language))
    };
    store.meetings[index].language = language.clone();
    store.meetings[index].updated_at = now_string();
    if let Err(error) = write_store(&state.data_dir, &store) {
        store.meetings[index] = previous;
        return Err(error.to_string());
    }
    let updated = store.meetings[index].clone();
    if let Some(active_language) = active_language.as_mut() {
        **active_language = language;
    }
    Ok(updated)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fatal_capture_failure_stops_native_audio_and_finalizes_the_new_wav() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("synthetic-recovery.wav");
        let mut wav = create_incremental_wav(&path, 16_000).unwrap();
        append_wav_chunk(&mut wav, &[0.25, -0.5, 0.75]).unwrap();
        let writer = Arc::new(Mutex::new(Some(wav)));
        let capture = Arc::new(Mutex::new(RecordingCapture::new(true, false)));
        let pending = Arc::new(Mutex::new(PendingAudio::default()));
        let (stop_tx, stop_rx) = std::sync::mpsc::channel();

        let message = finish_failed_recording(
            RecordingFailure::Capture("Synthetic capture overflow".into()),
            &capture, &writer, &pending, &stop_tx,
        );

        assert!(stop_rx.try_recv().is_ok());
        assert!(writer.lock().unwrap().is_none());
        assert_eq!(message, "Synthetic capture overflow");
        assert_eq!(captured_wav_duration(&path).unwrap(), 3.0 / 16_000.0);
        let mut reader = hound::WavReader::open(&path).unwrap();
        let saved: Vec<i16> = reader.samples::<i16>().map(|sample| sample.unwrap()).collect();
        assert_eq!(saved, [0.25, -0.5, 0.75].map(crate::audio::pcm16_sample));
        // The error and the later UI stop cannot finalize the writer twice.
        assert!(writer.lock().unwrap().take().is_none());
    }

    #[test]
    fn recording_error_contract_identifies_the_session_and_retained_recovery_file() {
        let path = Path::new("synthetic-new-recording.wav");
        let payload = recording_error_payload("meeting-test", "Disk write failed", path);
        assert_eq!(payload, serde_json::json!({
            "meetingId": "meeting-test",
            "message": "Disk write failed",
            "fatal": true,
            "recordingPath": "synthetic-new-recording.wav",
        }));
    }

    #[test]
    fn an_already_queued_fatal_stop_does_not_block_ui_cleanup() {
        let (stop_tx, mut stop_rx) = mpsc::channel(1);
        stop_tx.try_send(()).unwrap();
        assert!(matches!(stop_tx.try_send(()), Err(mpsc::error::TrySendError::Full(()))));
        assert_eq!(stop_rx.try_recv(), Ok(()));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn queued_utterance_keeps_language_snapshot_after_setting_changes() {
        let request = FoundryLocalTranscriptionRequest {
            model_alias: "test-model".into(), language: Some("en".into()),
        };
        let language = Mutex::new("KO_kr".to_string());
        let (sender, mut receiver) = mpsc::channel(2);
        let utterance = || SpeechUtterance {
            offset_samples: 16_000, samples: vec![0.0; 320], voiced_frames: 1,
        };
        enqueue_utterances(&sender, vec![utterance()], &request, &language).await.unwrap();
        *language.lock().unwrap() = "ja-JP".into();
        enqueue_utterances(&sender, vec![utterance()], &request, &language).await.unwrap();
        let first = receiver.recv().await.unwrap();
        let second = receiver.recv().await.unwrap();
        assert_eq!(first.request.language.as_deref(), Some("ko"));
        assert_eq!(second.request.language.as_deref(), Some("ja"));
        assert_eq!(first.request.model_alias, "test-model");
        assert_eq!(first.utterance.offset_samples, 16_000);
        assert_eq!(request.language.as_deref(), Some("en"));
        *language.lock().unwrap() = "auto".into();
        assert!(foundry_request_for_job(&request, &language).unwrap().language.is_none());
    }

    #[test]
    fn foundry_wav_duration_and_pcm_are_sample_based() {
        let samples = vec![0.12345; 32_001];
        let bytes = wav_bytes(&samples, 16_000).unwrap();
        let mut reader = hound::WavReader::new(Cursor::new(bytes)).unwrap();
        assert_eq!(reader.duration(), samples.len() as u32);
        assert_eq!(reader.duration() as f64 / f64::from(reader.spec().sample_rate), 2.0000625);
        assert!(reader.samples::<i16>().all(|sample| sample.unwrap() == crate::audio::pcm16_sample(0.12345)));
    }

    #[test]
    fn normalizes_transcription_engines() {
        for value in [
            "foundryLocal",
            "foundry_local",
            "foundry-local",
            "local",
            "other",
        ] {
            assert_eq!(
                normalize_transcription_engine(Some(value.to_string())),
                TranscriptionEngine::FoundryLocal
            );
        }
        assert_eq!(
            normalize_transcription_engine(Some("azure".to_string())),
            TranscriptionEngine::Azure
        );
        assert_eq!(
            normalize_transcription_engine(None),
            TranscriptionEngine::FoundryLocal
        );
    }

    #[test]
    fn normalizes_foundry_chunking_modes() {
        for value in ["fixed5Seconds", "fixed-five-seconds", "fixed"] {
            assert_eq!(
                normalize_foundry_chunking_mode(Some(value.to_string())),
                FoundryChunkingMode::FixedFiveSeconds
            );
        }
        assert_eq!(
            normalize_foundry_chunking_mode(None),
            FoundryChunkingMode::Utterance
        );
        assert_eq!(
            normalize_foundry_chunking_mode(Some("other".to_string())),
            FoundryChunkingMode::Utterance
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn waits_for_task_completion_without_blocking_the_runtime() {
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            tokio::task::yield_now().await;
            let _ = done_tx.send(Ok::<(), String>(()));
        });

        let result =
            await_task_completion(done_rx, "Transcription task", Duration::from_secs(1)).await;

        assert_eq!(result, Ok(()));
    }
}

use super::*;

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

// The flat argument list preserves the existing Tauri IPC payload.
#[allow(clippy::too_many_arguments)]
#[tauri::command(async)]
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
    let context = if transcription_engine == TranscriptionEngine::Local {
        Some(get_or_load_whisper_context(&state)?)
    } else {
        None
    };

    let language = normalize_language(language);
    let capture_mode = normalize_capture_mode(capture_mode);
    let capture_microphone = capture_mode != "system";
    let capture_system = capture_mode != "microphone";
    let recording_samples = Arc::new(Mutex::new(CaptureBuffers::default()));
    let pending = Arc::new(Mutex::new(CaptureBuffers::default()));
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
    let pending_for_audio = pending.clone();
    let audio_thread = std::thread::spawn(move || {
        match build_capture_streams(
            capture_microphone,
            capture_system,
            audio_device_id.as_deref(),
            system_audio_device_id.as_deref(),
            &capture_mode,
            recording_samples_for_audio,
            Some(pending_for_audio),
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

    let append_to_meeting_id = append_to_meeting_id
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty());
    let existing_meeting = if let Some(meeting_id) = append_to_meeting_id.as_deref() {
        let store = state
            .store
            .lock()
            .map_err(|_| "Store lock poisoned".to_string())?;
        Some(
            store
                .meetings
                .iter()
                .find(|meeting| meeting.id == meeting_id)
                .cloned()
                .ok_or_else(|| "The open meeting session no longer exists.".to_string())?,
        )
    } else {
        None
    };
    let engine = match transcription_engine {
        TranscriptionEngine::Local => "local".to_string(),
        TranscriptionEngine::Azure => "azure".to_string(),
    };
    let is_appending = existing_meeting.is_some();
    // When appending, keep the open session's transcript and existing audio and
    // just refresh the capture metadata for the new segment.
    let prior_recording_path = existing_meeting
        .as_ref()
        .and_then(|meeting| meeting.recording_path.as_deref())
        .map(PathBuf::from);
    let mut meeting = match existing_meeting {
        Some(mut meeting) => {
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

    let (stop_tx, mut stop_rx) = mpsc::channel::<()>(1);
    let meeting_id = meeting.id.clone();
    let app_for_task = app.clone();
    let pending_for_task = pending.clone();
    let recording_samples_for_task = recording_samples.clone();
    let audio_writer_for_task = audio_writer.clone();
    let language_for_task = Arc::new(Mutex::new(language));
    let language_for_recorder = language_for_task.clone();
    let initial_transcript_offset = meeting.duration_seconds;

    tauri::async_runtime::spawn(async move {
        let interval_duration = if transcription_engine == TranscriptionEngine::Azure {
            Duration::from_millis(250)
        } else {
            Duration::from_secs(TRANSCRIPT_CHUNK_SECONDS)
        };
        let mut interval = tokio::time::interval(interval_duration);
        let mut offset_seconds = initial_transcript_offset;
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    let audio_chunk = recording_samples_for_task
                        .lock()
                        .map(|mut buffer| CaptureBuffers {
                            microphone: std::mem::take(&mut buffer.microphone),
                            system: std::mem::take(&mut buffer.system),
                        })
                        .unwrap_or_default();
                    let mixed_audio = mix_sources(&audio_chunk.microphone, &audio_chunk.system);
                    if !mixed_audio.is_empty() {
                        if let Ok(mut writer) = audio_writer_for_task.lock() {
                            if let Some(writer) = writer.as_mut() {
                                let _ = append_wav_chunk(writer, &mixed_audio);
                            }
                        }
                    }
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

                    let chunk_language = language_for_task
                        .lock()
                        .map(|language| language.clone())
                        .unwrap_or_else(|_| crate::default_language());
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
                            let _ = append_segment_to_store(&app_for_task, &meeting_id, segment.clone());
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
        prior_recording_path,
        stop_tx,
        audio_stop_tx,
        audio_thread,
        audio_done_rx,
        recording_samples,
        audio_writer,
        recording_path,
        language: language_for_recorder,
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

#[tauri::command(async)]
pub(crate) fn stop_recording(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<Meeting, String> {
    let _lifecycle_guard = state
        .recording_lifecycle_lock
        .lock()
        .map_err(|_| "Recording lifecycle lock poisoned".to_string())?;
    let session = state
        .recorder
        .lock()
        .map_err(|_| "Recorder lock poisoned".to_string())?
        .take()
        .ok_or_else(|| "Recording is not running".to_string())?;

    let _ = session.stop_tx.try_send(());
    let _ = session.audio_stop_tx.send(());
    if session
        .audio_done_rx
        .recv_timeout(AUDIO_THREAD_STOP_TIMEOUT)
        .is_ok()
    {
        let _ = session.audio_thread.join();
    } else {
        eprintln!(
            "Audio capture thread did not stop within {} seconds; continuing finalization.",
            AUDIO_THREAD_STOP_TIMEOUT.as_secs()
        );
    }

    let final_chunk = session
        .recording_samples
        .lock()
        .map(|mut buffer| CaptureBuffers {
            microphone: std::mem::take(&mut buffer.microphone),
            system: std::mem::take(&mut buffer.system),
        })
        .map_err(|_| "Sample buffer lock poisoned".to_string())?;
    let final_samples = mix_sources(&final_chunk.microphone, &final_chunk.system);
    {
        let mut writer = session
            .audio_writer
            .lock()
            .map_err(|_| "Audio writer lock poisoned".to_string())?;
        if let Some(mut writer) = writer.take() {
            append_wav_chunk(&mut writer, &final_samples).map_err(|error| error.to_string())?;
            writer.finalize().map_err(|error| error.to_string())?;
        }
    }
    let duration = session.started_at.elapsed().as_secs_f64();
    let encoded_path = session.recording_path.with_extension("mp3");
    video::encode_mp3(&app, &session.recording_path, &encoded_path)
        .map_err(|error| format!("Could not save the meeting audio as MP3: {error}"))?;
    // When appending, concatenate the prior meeting audio with the new segment
    // into a fresh file; otherwise the encoded MP3 is the final recording.
    let recording_path = match session.prior_recording_path.as_deref() {
        Some(previous_path) if previous_path.is_file() => {
            let merged_path = state.data_dir.join("recordings").join(format!(
                "{}-{}.mp3",
                session.meeting_id,
                Uuid::new_v4()
            ));
            video::concatenate_audio(&app, previous_path, &encoded_path, &merged_path)
                .map_err(|error| format!("Could not append the meeting audio: {error}"))?;
            merged_path
        }
        _ => encoded_path.clone(),
    };
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
        meeting.duration_seconds += duration;
        meeting.has_audio = true;
        meeting.recording_path = Some(recording_path.to_string_lossy().to_string());
        meeting.updated_at = now_string();
        meeting.clone()
    };
    state.save_store().map_err(|error| error.to_string())?;
    // Remove the raw WAV and any intermediate files the final MP3 superseded.
    let _ = fs::remove_file(&session.recording_path);
    if encoded_path != recording_path {
        let _ = fs::remove_file(&encoded_path);
    }
    if let Some(previous_path) = session.prior_recording_path {
        if previous_path != recording_path {
            let _ = fs::remove_file(previous_path);
        }
    }
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
    if let Ok(recorder) = state.recorder.lock() {
        if let Some(session) = recorder
            .as_ref()
            .filter(|session| session.meeting_id == meeting_id)
        {
            if let Ok(mut active_language) = session.language.lock() {
                *active_language = language;
            }
        }
    }
    state.save_store().map_err(|error| error.to_string())?;
    Ok(updated)
}

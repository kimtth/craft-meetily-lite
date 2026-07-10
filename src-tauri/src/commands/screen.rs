use super::*;

fn make_video(
    id: String,
    title: Option<String>,
    target_name: String,
    codec: String,
    video_path: String,
    audio_path: Option<String>,
) -> VideoRecording {
    let now = now_string();
    let has_audio = audio_path.is_some();
    VideoRecording {
        id,
        title: title.unwrap_or_else(|| {
            format!(
                "Screen recording {}",
                chrono::Local::now().format("%Y-%m-%d %H-%M")
            )
        }),
        created_at: now.clone(),
        updated_at: now,
        duration_seconds: 0.0,
        status: "starting".to_string(),
        target_name,
        codec,
        video_path,
        has_audio: Some(has_audio),
        audio_path,
        post_process_stage: Some("initializing-capture".to_string()),
    }
}

fn final_video_path(provisional: &Path) -> PathBuf {
    provisional.with_file_name(
        provisional
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("screen.partial.mp4")
            .replace(".partial.mp4", ".mp4"),
    )
}

fn save_video_recording(state: &AppState, updated: &VideoRecording) -> Result<(), String> {
    let mut store = state
        .store
        .lock()
        .map_err(|_| "Store lock poisoned".to_string())?;
    let video = store
        .videos
        .iter_mut()
        .find(|video| video.id == updated.id)
        .ok_or_else(|| "Video recording not found".to_string())?;
    *video = updated.clone();
    write_store(&state.data_dir, &store).map_err(|error| error.to_string())
}

fn remove_video_recording(state: &AppState, video_id: &str) -> Result<(), String> {
    let mut store = state
        .store
        .lock()
        .map_err(|_| "Store lock poisoned".to_string())?;
    store.videos.retain(|video| video.id != video_id);
    write_store(&state.data_dir, &store).map_err(|error| error.to_string())
}

fn mark_post_processing_failed(video: &mut VideoRecording, stage: &str) {
    video.status = "post-processing-failed".to_string();
    video.post_process_stage = Some(stage.to_string());
    video.updated_at = now_string();
}

fn mark_capture_failed(video: &mut VideoRecording, stage: &str) {
    video.status = "capture-failed".to_string();
    video.post_process_stage = Some(stage.to_string());
    video.updated_at = now_string();
}

fn has_recorded_video_data(video: &VideoRecording) -> bool {
    fs::metadata(&video.video_path)
        .map(|metadata| metadata.is_file() && metadata.len() > 0)
        .unwrap_or(false)
}

fn emit_screen_processing_status(app: &AppHandle, stage: &str, detail: &str) {
    let _ = app.emit(
        "screen-processing-status",
        ScreenProcessingStatusEvent {
            stage: stage.to_string(),
            detail: detail.to_string(),
        },
    );
}

fn finalize_screen_recording(app: &AppHandle, video: &mut VideoRecording) -> Result<()> {
    let provisional = PathBuf::from(&video.video_path);
    if !has_recorded_video_data(video) {
        return Err(anyhow!(
            "The screen recording is empty. FFmpeg stopped before it encoded a video frame."
        ));
    }

    let final_path = final_video_path(&provisional);
    let staged_audio = video
        .audio_path
        .as_deref()
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_file());

    if let Some(audio_path) = staged_audio {
        emit_screen_processing_status(app, "Mixing audio", "Adding captured audio to the video.");
        let muxed_path = final_path.with_file_name(format!("{}.muxing.mp4", video.id));
        let _ = fs::remove_file(&muxed_path);
        video::mux_audio(app, &provisional, &audio_path, &muxed_path)?;
        emit_screen_processing_status(
            app,
            "Validating recording",
            "Checking the completed video file.",
        );
        video::validate_recording(app, &muxed_path)?;
        emit_screen_processing_status(app, "Saving recording", "Committing the completed video.");
        fs::rename(&muxed_path, &final_path)
            .with_context(|| format!("Could not finalize {}", final_path.display()))?;
        let _ = fs::remove_file(&audio_path);
        video.has_audio = Some(true);
    } else {
        emit_screen_processing_status(
            app,
            "Validating recording",
            "Checking the completed video file.",
        );
        video::validate_recording(app, &provisional)?;
        emit_screen_processing_status(app, "Saving recording", "Committing the completed video.");
        fs::rename(&provisional, &final_path)
            .with_context(|| format!("Could not finalize {}", final_path.display()))?;
        video.has_audio = Some(false);
    }

    let _ = fs::remove_file(&provisional);
    video.video_path = final_path.to_string_lossy().to_string();
    video.audio_path = None;
    video.status = "saved".to_string();
    video.post_process_stage = None;
    video.updated_at = now_string();
    Ok(())
}

pub(crate) fn recover_incomplete_screen_recordings(
    app: &AppHandle,
    state: &AppState,
) -> Result<()> {
    let active_video_id = state
        .screen_recorder
        .lock()
        .map_err(|_| anyhow!("Screen recorder lock poisoned"))?
        .as_ref()
        .map(|session| session.video_id.clone());
    let pending = state
        .store
        .lock()
        .map_err(|_| anyhow!("Store lock poisoned"))?
        .videos
        .iter()
        .filter(|video| {
            active_video_id.as_deref() != Some(video.id.as_str())
                && matches!(
                    video.status.as_str(),
                    "starting" | "recording" | "post-processing" | "post-processing-failed"
                )
                && video.video_path.ends_with(".partial.mp4")
        })
        .cloned()
        .collect::<Vec<_>>();

    for mut video in pending {
        if !has_recorded_video_data(&video) {
            mark_capture_failed(&mut video, "empty-video-output");
        } else if let Err(error) = finalize_screen_recording(app, &mut video) {
            mark_post_processing_failed(&mut video, "retry-needed");
            eprintln!("Could not recover screen recording {}: {error}", video.id);
        }
        save_video_recording(state, &video).map_err(|error| anyhow!(error))?;
    }
    Ok(())
}

#[tauri::command]
pub(crate) fn get_videos(state: tauri::State<'_, AppState>) -> Result<Vec<VideoRecording>, String> {
    let store = state
        .store
        .lock()
        .map_err(|_| "Store lock poisoned".to_string())?;
    Ok(store.videos.clone())
}

#[tauri::command(async)]
pub(crate) fn get_recording_runtime_status(
    state: tauri::State<'_, AppState>,
) -> Result<RecordingRuntimeStatus, String> {
    let _lifecycle_guard = state
        .recording_lifecycle_lock
        .lock()
        .map_err(|_| "Recording lifecycle lock poisoned".to_string())?;
    let recorder = state
        .recorder
        .lock()
        .map_err(|_| "Recorder lock poisoned".to_string())?;
    let screen_recorder = state
        .screen_recorder
        .lock()
        .map_err(|_| "Screen recorder lock poisoned".to_string())?;
    Ok(RecordingRuntimeStatus {
        audio_recording_active: recorder.is_some(),
        audio_meeting_id: recorder.as_ref().map(|session| session.meeting_id.clone()),
        audio_elapsed_seconds: recorder
            .as_ref()
            .map(|session| session.started_at.elapsed().as_secs_f64())
            .unwrap_or_default(),
        screen_recording_active: screen_recorder.is_some(),
        screen_video_id: screen_recorder
            .as_ref()
            .map(|session| session.video_id.clone()),
        screen_elapsed_seconds: screen_recorder
            .as_ref()
            .map(|session| session.started_at.elapsed().as_secs_f64())
            .unwrap_or_default(),
    })
}

#[tauri::command(async)]
pub(crate) fn refresh_videos(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<VideoRecording>, String> {
    let _lifecycle_guard = state
        .recording_lifecycle_lock
        .lock()
        .map_err(|_| "Recording lifecycle lock poisoned".to_string())?;
    recover_incomplete_screen_recordings(&app, &state).map_err(|error| error.to_string())?;
    let output_folder = video_output_folder(&state)?;
    fs::create_dir_all(&output_folder).map_err(|error| error.to_string())?;
    let existing = state
        .store
        .lock()
        .map_err(|_| "Store lock poisoned".to_string())?
        .videos
        .clone();
    let mut videos = Vec::new();

    for entry in fs::read_dir(&output_folder).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let path = entry.path();
        let is_mp4 = path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("mp4"));
        let is_intermediate = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(".partial.mp4") || name.ends_with(".muxing.mp4"));
        if !is_mp4 || is_intermediate || !path.is_file() {
            continue;
        }

        let path = path.to_string_lossy().to_string();
        if let Some(video) = existing
            .iter()
            .find(|video| video.video_path.eq_ignore_ascii_case(&path))
        {
            videos.push(video.clone());
            continue;
        }
        if entry
            .metadata()
            .map(|metadata| metadata.len() == 0)
            .unwrap_or(true)
        {
            continue;
        }

        let stem = Path::new(&path)
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or("Screen recording");
        let created_at = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .map(chrono::DateTime::<Utc>::from)
            .map(|timestamp| timestamp.to_rfc3339())
            .unwrap_or_else(|_| now_string());
        videos.push(VideoRecording {
            id: format!("video-file-{}", Uuid::new_v4()),
            title: stem.replace('-', " "),
            created_at: created_at.clone(),
            updated_at: created_at,
            duration_seconds: 0.0,
            status: "saved".to_string(),
            target_name: "Imported video file".to_string(),
            codec: "mp4".to_string(),
            video_path: path,
            has_audio: None,
            audio_path: None,
            post_process_stage: None,
        });
    }

    for video in existing {
        let already_included = videos.iter().any(|current| {
            current.id == video.id || current.video_path.eq_ignore_ascii_case(&video.video_path)
        });
        // Keep in-progress or failed recordings, whose partial files are not
        // scanned as ordinary videos, so their metadata survives a refresh.
        if !already_included && video.status != "saved" {
            videos.push(video);
        }
    }

    videos.sort_by(|left, right| right.created_at.cmp(&left.created_at));
    let mut store = state
        .store
        .lock()
        .map_err(|_| "Store lock poisoned".to_string())?;
    store.videos = videos.clone();
    write_store(&state.data_dir, &store).map_err(|error| error.to_string())?;
    Ok(videos)
}

#[tauri::command]
pub(crate) fn list_screen_targets() -> Vec<ScreenTarget> {
    video::list_screen_targets()
}

#[tauri::command]
pub(crate) async fn open_area_selector(app: AppHandle) -> Result<(), String> {
    if let Some(window) = app.get_webview_window("area-selector") {
        window.show().map_err(|error| error.to_string())?;
        window.set_focus().map_err(|error| error.to_string())?;
        return Ok(());
    }

    let desktop = video::list_screen_targets()
        .into_iter()
        .next()
        .ok_or_else(|| "No display is available for area selection.".to_string())?;
    let selector = WebviewWindowBuilder::new(
        &app,
        "area-selector",
        WebviewUrl::App("index.html?screenSelector=true".into()),
    )
    .title("Select recording area")
    .decorations(false)
    .transparent(true)
    .always_on_top(true)
    .skip_taskbar(true)
    .resizable(false)
    .build()
    .map_err(|error| error.to_string())?;
    selector
        .set_position(PhysicalPosition::new(desktop.x, desktop.y))
        .map_err(|error| error.to_string())?;
    selector
        .set_size(PhysicalSize::new(
            desktop.width as u32,
            desktop.height as u32,
        ))
        .map_err(|error| error.to_string())?;
    Ok(())
}

#[tauri::command]
pub(crate) fn complete_area_selection(
    app: AppHandle,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    viewport_width: f64,
    viewport_height: f64,
) -> Result<(), String> {
    if !width.is_finite()
        || !height.is_finite()
        || !x.is_finite()
        || !y.is_finite()
        || !viewport_width.is_finite()
        || !viewport_height.is_finite()
        || width < 12.0
        || height < 12.0
        || viewport_width <= 0.0
        || viewport_height <= 0.0
    {
        return Err("Drag a valid recording area before releasing the mouse button.".to_string());
    }
    let selector = app
        .get_webview_window("area-selector")
        .ok_or_else(|| "The area selector is not open.".to_string())?;
    let position = selector
        .outer_position()
        .map_err(|error| error.to_string())?;
    let size = selector.inner_size().map_err(|error| error.to_string())?;
    let scale_x = size.width as f64 / viewport_width;
    let scale_y = size.height as f64 / viewport_height;
    let target_x = (x.clamp(0.0, viewport_width) * scale_x).round() as i32;
    let target_y = (y.clamp(0.0, viewport_height) * scale_y).round() as i32;
    let target_width = (width.min(viewport_width - x.clamp(0.0, viewport_width)) * scale_x)
        .round()
        .max(1.0) as i32;
    let target_height = (height.min(viewport_height - y.clamp(0.0, viewport_height)) * scale_y)
        .round()
        .max(1.0) as i32;
    let (target_width, target_height) =
        video::normalize_yuv420_dimensions(target_width, target_height)
            .map_err(|error| error.to_string())?;
    let target = ScreenTarget {
        id: "area".to_string(),
        name: format!("Selected area ({} × {})", target_width, target_height),
        x: position.x + target_x,
        y: position.y + target_y,
        width: target_width,
        height: target_height,
    };
    selector.close().map_err(|error| error.to_string())?;
    app.emit("area-selected", target)
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn cancel_area_selection(app: AppHandle) -> Result<(), String> {
    if let Some(selector) = app.get_webview_window("area-selector") {
        selector.close().map_err(|error| error.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub(crate) fn select_video_output_folder(app: AppHandle) -> Result<Option<String>, String> {
    Ok(app
        .dialog()
        .file()
        .blocking_pick_folder()
        .map(|folder| folder.to_string()))
}

#[tauri::command]
pub(crate) fn select_ffmpeg_executable(app: AppHandle) -> Result<Option<String>, String> {
    Ok(app
        .dialog()
        .file()
        .add_filter("FFmpeg executable", &["exe"])
        .blocking_pick_file()
        .map(|file| file.to_string()))
}

fn video_output_folder(state: &AppState) -> Result<PathBuf, String> {
    let store = state
        .store
        .lock()
        .map_err(|_| "Store lock poisoned".to_string())?;
    if store.settings.video_output_folder.trim().is_empty() {
        Ok(state.data_dir.join("recordings"))
    } else {
        Ok(PathBuf::from(&store.settings.video_output_folder))
    }
}

#[tauri::command]
pub(crate) fn open_video_recordings_folder(
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let output_folder = video_output_folder(&state)?;
    fs::create_dir_all(&output_folder).map_err(|error| error.to_string())?;
    open_path_in_explorer(&output_folder).map_err(|error| error.to_string())
}

fn audio_level(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let mean_square =
        samples.iter().map(|sample| sample * sample).sum::<f32>() / samples.len() as f32;
    mean_square.sqrt().clamp(0.0, 1.0)
}

fn start_screen_audio_capture(
    app: AppHandle,
    audio_path: PathBuf,
    capture_microphone: bool,
    capture_system: bool,
    audio_device_id: Option<String>,
    system_audio_device_id: Option<String>,
    capture_mode: String,
) -> Result<ScreenAudioCaptureHandle, String> {
    let (setup_tx, setup_rx) = std::sync::mpsc::channel::<Result<(), String>>();
    let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
    let audio_thread = std::thread::spawn(move || {
        let samples = Arc::new(Mutex::new(CaptureBuffers::default()));
        let streams = match build_capture_streams(
            capture_microphone,
            capture_system,
            audio_device_id.as_deref(),
            system_audio_device_id.as_deref(),
            &capture_mode,
            samples.clone(),
            None,
        ) {
            Ok((streams, _)) => streams,
            Err(error) => {
                let _ = setup_tx.send(Err(error));
                return;
            }
        };
        let mut writer = match create_incremental_wav(&audio_path, 16_000) {
            Ok(writer) => writer,
            Err(error) => {
                let _ = setup_tx.send(Err(error.to_string()));
                return;
            }
        };
        if setup_tx.send(Ok(())).is_err() {
            return;
        }

        loop {
            let should_stop = match stop_rx.recv_timeout(Duration::from_millis(100)) {
                Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => true,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => false,
            };
            let chunk = samples
                .lock()
                .map(|mut buffer| CaptureBuffers {
                    microphone: std::mem::take(&mut buffer.microphone),
                    system: std::mem::take(&mut buffer.system),
                })
                .unwrap_or_default();
            let mixed = mix_sources(&chunk.microphone, &chunk.system);
            if !mixed.is_empty() {
                let _ = append_wav_chunk(&mut writer, &mixed);
            }
            let _ = app.emit(
                "screen-audio-level",
                ScreenAudioLevelEvent {
                    microphone: audio_level(&chunk.microphone),
                    system: audio_level(&chunk.system),
                    mixed: audio_level(&mixed),
                },
            );
            if should_stop {
                break;
            }
        }
        drop(streams);
        let _ = writer.finalize();
    });
    match setup_rx.recv_timeout(AUDIO_DEVICE_SETUP_TIMEOUT) {
        Ok(Ok(())) => Ok((stop_tx, audio_thread)),
        Ok(Err(error)) => {
            let _ = audio_thread.join();
            Err(error)
        }
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Err(format!(
            "Screen audio devices did not initialize within {} seconds.",
            AUDIO_DEVICE_SETUP_TIMEOUT.as_secs()
        )),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            let _ = audio_thread.join();
            Err("Screen audio capture thread stopped unexpectedly".to_string())
        }
    }
}

fn cancel_screen_audio_capture((stop_tx, thread): ScreenAudioCaptureHandle) {
    let _ = stop_tx.send(());
    let _ = thread.join();
}

// The flat argument list preserves the existing Tauri IPC payload.
#[allow(clippy::too_many_arguments)]
#[tauri::command(async)]
pub(crate) fn start_screen_recording(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    title: Option<String>,
    target_id: String,
    target_name: String,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    codec: String,
    audio_device_id: Option<String>,
    system_audio_device_id: Option<String>,
    capture_mode: Option<String>,
) -> Result<VideoRecording, String> {
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
        return Err("Stop the current recording before starting a screen recording.".to_string());
    }
    let (width, height) =
        video::normalize_yuv420_dimensions(width, height).map_err(|error| error.to_string())?;
    let codec = if codec == "h265" { "h265" } else { "h264" }.to_string();
    let capture_mode = match capture_mode.as_deref() {
        Some("none") => "none".to_string(),
        _ => normalize_capture_mode(capture_mode),
    };
    let capture_microphone = capture_mode != "system" && capture_mode != "none";
    let capture_system = capture_mode != "microphone" && capture_mode != "none";
    let output_folder = video_output_folder(&state)?;
    fs::create_dir_all(&output_folder).map_err(|error| error.to_string())?;
    let recording_key = Uuid::new_v4().to_string();
    let video_id = format!("video-{recording_key}");
    let provisional = output_folder.join(format!("screen-{recording_key}.partial.mp4"));
    let audio_path = if capture_mode == "none" {
        None
    } else {
        let audio_staging_folder = std::env::temp_dir().join("MeetlyLite").join("screen-audio");
        fs::create_dir_all(&audio_staging_folder).map_err(|error| error.to_string())?;
        Some(audio_staging_folder.join(format!("screen-audio-{recording_key}.wav")))
    };
    let target_name = if target_id == "area" {
        format!("Selected area ({} × {})", width, height)
    } else {
        target_name
    };
    let mut video = make_video(
        video_id,
        title,
        target_name.clone(),
        codec.clone(),
        provisional.to_string_lossy().to_string(),
        audio_path
            .as_ref()
            .map(|path| path.to_string_lossy().to_string()),
    );
    let initial_persist = {
        let mut store = state
            .store
            .lock()
            .map_err(|_| "Store lock poisoned".to_string())?;
        store.videos.insert(0, video.clone());
        let persist = write_store(&state.data_dir, &store);
        if persist.is_err() {
            store.videos.retain(|item| item.id != video.id);
        }
        persist
    };
    if let Err(error) = initial_persist {
        return Err(error.to_string());
    }

    let audio_capture = match audio_path.as_ref() {
        Some(audio_path) => match start_screen_audio_capture(
            app.clone(),
            audio_path.clone(),
            capture_microphone,
            capture_system,
            audio_device_id,
            system_audio_device_id,
            capture_mode,
        ) {
            Ok(capture) => Some(capture),
            Err(error) => {
                let _ = fs::remove_file(audio_path);
                let _ = remove_video_recording(&state, &video.id);
                return Err(error);
            }
        },
        None => None,
    };
    let target = ScreenTarget {
        id: target_id,
        name: target_name.clone(),
        x,
        y,
        width,
        height,
    };
    let capture = match video::start_capture(&app, &target, &codec, &provisional) {
        Ok(capture) => capture,
        Err(error) => {
            let _ = fs::remove_file(&provisional);
            if let Some(audio_capture) = audio_capture {
                cancel_screen_audio_capture(audio_capture);
            }
            if let Some(audio_path) = audio_path.as_ref() {
                let _ = fs::remove_file(audio_path);
            }
            let _ = remove_video_recording(&state, &video.id);
            return Err(error.to_string());
        }
    };
    let capture_started_at = std::time::Instant::now();
    video.status = "recording".to_string();
    video.post_process_stage = Some("capturing".to_string());
    video.updated_at = now_string();
    if let Err(error) = save_video_recording(&state, &video) {
        let mut capture = capture;
        let _ = video::stop_capture(&mut capture);
        if let Some(audio_capture) = audio_capture {
            cancel_screen_audio_capture(audio_capture);
        }
        if let Some(audio_path) = audio_path.as_ref() {
            let _ = fs::remove_file(audio_path);
        }
        let _ = fs::remove_file(&provisional);
        let _ = remove_video_recording(&state, &video.id);
        return Err(error);
    }
    let (audio_stop_tx, audio_thread) = match audio_capture {
        Some((stop_tx, thread)) => (Some(stop_tx), Some(thread)),
        None => (None, None),
    };
    *state
        .screen_recorder
        .lock()
        .map_err(|_| "Screen recorder lock poisoned".to_string())? = Some(ScreenRecorderSession {
        video_id: video.id.clone(),
        capture,
        audio_stop_tx,
        audio_thread,
        started_at: capture_started_at,
    });
    Ok(video)
}

#[tauri::command(async)]
pub(crate) fn stop_screen_recording(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<VideoRecording, String> {
    let _lifecycle_guard = state
        .recording_lifecycle_lock
        .lock()
        .map_err(|_| "Recording lifecycle lock poisoned".to_string())?;
    let mut session = state
        .screen_recorder
        .lock()
        .map_err(|_| "Screen recorder lock poisoned".to_string())?
        .take()
        .ok_or_else(|| "Screen recording is not running".to_string())?;
    emit_screen_processing_status(&app, "Stopping capture", "Closing the screen video stream.");
    let capture_warning = video::stop_capture(&mut session.capture)
        .err()
        .map(|error| error.to_string());
    if let Some(audio_stop_tx) = session.audio_stop_tx.take() {
        let _ = audio_stop_tx.send(());
    }
    if let Some(audio_thread) = session.audio_thread.take() {
        let _ = audio_thread.join();
    }
    let mut updated = state
        .store
        .lock()
        .map_err(|_| "Store lock poisoned".to_string())?
        .videos
        .iter()
        .find(|video| video.id == session.video_id)
        .cloned()
        .ok_or_else(|| "Video recording not found".to_string())?;
    updated.duration_seconds = session.started_at.elapsed().as_secs_f64();

    if let Err(error) = finalize_screen_recording(&app, &mut updated) {
        if has_recorded_video_data(&updated) {
            mark_post_processing_failed(&mut updated, "retry-needed");
        } else {
            mark_capture_failed(&mut updated, "empty-video-output");
        }
        save_video_recording(&state, &updated)?;
        let capture_detail = capture_warning
            .as_deref()
            .map(|warning| format!(" FFmpeg also reported: {warning}"))
            .unwrap_or_default();
        return Err(format!(
            "Screen recording stopped, but it could not be finalized: {error}.{capture_detail}"
        ));
    }
    if let Some(warning) = capture_warning {
        eprintln!(
            "FFmpeg reported a non-zero screen-capture status, but the finalized recording validated successfully: {warning}"
        );
    }
    save_video_recording(&state, &updated)?;
    Ok(updated)
}

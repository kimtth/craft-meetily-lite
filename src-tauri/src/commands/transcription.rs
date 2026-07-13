use super::*;

#[tauri::command]
pub(crate) fn select_fast_transcription_audio(
    app: AppHandle,
) -> Result<Option<serde_json::Value>, String> {
    let Some(path) = app
        .dialog()
        .file()
        .add_filter("WAV, MP3, or MP4", &["wav", "mp3", "mp4"])
        .blocking_pick_file()
    else {
        return Ok(None);
    };
    let path = PathBuf::from(path.to_string());
    let (extension, size_bytes, duration) = fast_transcription_file_details(&path)?;
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("audio")
        .to_string();
    Ok(Some(serde_json::json!({
        "path": path.to_string_lossy(),
        "name": name,
        "sizeBytes": size_bytes,
        "durationSeconds": duration.as_secs_f64(),
        "requiresMp3Extraction": extension == "mp4",
    })))
}

// The flat argument list preserves the existing Tauri IPC payload.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub(crate) async fn transcribe_fast_audio(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    path: String,
    title: Option<String>,
    language: String,
    endpoint: String,
    tenant_id: Option<String>,
    subscription_id: Option<String>,
) -> Result<Meeting, String> {
    let mut source = PathBuf::from(path);
    let _ = app.emit(
        "fast-transcription-progress",
        serde_json::json!({
            "phase": "validating", "detail": "Validating the selected media file."
        }),
    );
    let (mut extension, _, mut source_duration) = fast_transcription_file_details(&source)?;
    let endpoint = fast_transcription_endpoint(&endpoint)?;
    let locale = language.trim().to_string();
    if locale.is_empty() || locale == "auto" {
        return Err(
            "Choose an Azure Speech language before starting file transcription.".to_string(),
        );
    }
    let mut temporary_source = None;
    if extension == "mp4" {
        let _ = app.emit("fast-transcription-progress", serde_json::json!({
            "phase": "validating", "detail": "Extracting MP3 locally from the MP4 before Azure Speech upload."
        }));
        let staging_folder = std::env::temp_dir()
            .join("MeetlyLite")
            .join("fast-transcription");
        fs::create_dir_all(&staging_folder).map_err(|error| error.to_string())?;
        let extracted = staging_folder.join(format!("fast-source-{}.mp3", Uuid::new_v4()));
        let extracted_guard = TemporaryMediaFile::new(extracted.clone());
        video::encode_mp3(&app, &source, &extracted).map_err(|error| error.to_string())?;
        source = extracted;
        temporary_source = Some(extracted_guard);
        (extension, _, source_duration) = fast_transcription_file_details(&source)?;
    }

    let token = azure_auth::get_azure_cli_access_token(tenant_id, subscription_id).await?;
    let audio = fs::read(&source)
        .map_err(|error| format!("Could not open the selected audio file: {error}"))?;
    let file_name = source
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("audio")
        .to_string();
    let mime_type = if extension == "mp3" {
        "audio/mpeg"
    } else {
        "audio/wav"
    };
    let definition = serde_json::json!({ "locales": [locale] }).to_string();
    let form = reqwest::multipart::Form::new()
        .part(
            "audio",
            reqwest::multipart::Part::bytes(audio)
                .file_name(file_name)
                .mime_str(mime_type)
                .map_err(|error| error.to_string())?,
        )
        .text("definition", definition);
    let _ = app.emit(
        "fast-transcription-progress",
        serde_json::json!({
            "phase": "uploading", "detail": "Uploading audio directly to Azure Speech."
        }),
    );
    let _ = app.emit(
        "fast-transcription-progress",
        serde_json::json!({
            "phase": "transcribing", "detail": "Azure Speech is transcribing the audio."
        }),
    );
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(30))
        .timeout(Duration::from_secs(2 * 60 * 60))
        .build()
        .map_err(|error| format!("Could not configure Azure Speech networking: {error}"))?;
    let response = client
        .post(endpoint)
        .bearer_auth(token.token)
        .multipart(form)
        .send()
        .await
        .map_err(|error| format!("Azure Speech Fast Transcription request failed: {error}"))?;
    let status = response.status();
    let body = response.text().await.map_err(|error| error.to_string())?;
    if !status.is_success() {
        return Err(format!(
            "Azure Speech Fast Transcription failed ({status}): {body}"
        ));
    }
    let result: serde_json::Value = serde_json::from_str(&body).map_err(|error| {
        format!("Azure Speech returned an invalid transcription response: {error}")
    })?;
    let transcript = result
        .get("phrases")
        .and_then(serde_json::Value::as_array)
        .map(|phrases| {
            phrases
                .iter()
                .filter_map(|phrase| {
                    let text = phrase.get("text")?.as_str()?.trim();
                    if text.is_empty() {
                        return None;
                    }
                    Some(TranscriptSegment {
                        id: format!("segment-{}", Uuid::new_v4()),
                        offset_seconds: phrase
                            .get("offsetMilliseconds")
                            .and_then(serde_json::Value::as_f64)
                            .unwrap_or_default()
                            / 1_000.0,
                        text: text.to_string(),
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let _ = app.emit(
        "fast-transcription-progress",
        serde_json::json!({
            "phase": "saving", "detail": "Saving audio and transcript locally."
        }),
    );
    let recording_path =
        state
            .data_dir
            .join("recordings")
            .join(format!("fast-{}.{}", Uuid::new_v4(), extension));
    let recording_guard = TemporaryMediaFile::new(recording_path.clone());
    fs::copy(&source, &recording_path).map_err(|error| error.to_string())?;
    let duration_seconds = result
        .get("durationMilliseconds")
        .and_then(serde_json::Value::as_f64)
        .map(|milliseconds| milliseconds / 1_000.0)
        .unwrap_or_else(|| source_duration.as_secs_f64());
    let mut meeting = make_meeting(MeetingOptions {
        title,
        audio_device_id: None,
        audio_device_name: None,
        system_audio_device_id: None,
        system_audio_device_name: None,
        capture_mode: "file".to_string(),
        language: locale,
        transcription_engine: "azure-fast".to_string(),
    });
    meeting.duration_seconds = duration_seconds;
    meeting.transcript = transcript;
    meeting.has_audio = true;
    meeting.recording_path = Some(recording_path.to_string_lossy().to_string());
    let persist_result = {
        let mut store = state
            .store
            .lock()
            .map_err(|_| "Store lock poisoned".to_string())?;
        store.meetings.insert(0, meeting.clone());
        let persist = write_store(&state.data_dir, &store);
        if persist.is_err() {
            store.meetings.retain(|item| item.id != meeting.id);
        }
        persist
    };
    persist_result.map_err(|error| error.to_string())?;
    recording_guard.keep();
    drop(temporary_source);
    Ok(meeting)
}

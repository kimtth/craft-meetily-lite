use super::*;

// Pure preflight: this must run before media inspection, extraction, or authentication.
fn fast_transcription_definition(
    language: &str,
    diarization: Option<bool>,
    upload_consent: Option<bool>,
) -> Result<serde_json::Value, String> {
    if upload_consent != Some(true) {
        return Err(
            "Explicit consent is required to upload audio to Azure Speech; Azure charges may apply."
                .to_string(),
        );
    }
    let locale = language.trim();
    if locale.is_empty() || locale.eq_ignore_ascii_case("auto") {
        return Err(
            "Choose an Azure Speech language before starting file transcription.".to_string(),
        );
    }
    let mut definition = serde_json::json!({ "locales": [locale] });
    if diarization == Some(true) {
        definition["diarization"] = serde_json::json!({ "enabled": true });
    }
    Ok(definition)
}

#[derive(Debug, PartialEq)]
struct FastTranscriptionPhrase {
    offset_seconds: f64,
    text: String,
    speaker_id: Option<String>,
}

fn fast_transcription_milliseconds(
    object: &serde_json::Value,
    field: &str,
    context: &str,
) -> Result<f64, String> {
    object
        .get(field)
        .and_then(serde_json::Value::as_f64)
        .filter(|value| value.is_finite() && *value >= 0.0)
        .ok_or_else(|| {
            format!("Azure Speech returned missing or invalid {field} in {context}.")
        })
}

// Parsing has no filesystem, network, or random-ID side effects. Do not substitute
// combinedPhrases: that would fabricate zero-offset segments and lose speaker timing.
fn parse_fast_transcription_phrases(
    result: &serde_json::Value,
) -> Result<Vec<FastTranscriptionPhrase>, String> {
    let phrases = result
        .get("phrases")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "Azure Speech returned missing or invalid phrases.".to_string())?;
    let mut transcript = Vec::with_capacity(phrases.len());
    for (index, phrase) in phrases.iter().enumerate() {
        let context = format!("phrase {}", index + 1);
        let offset = fast_transcription_milliseconds(phrase, "offsetMilliseconds", &context)?;
        let duration = fast_transcription_milliseconds(phrase, "durationMilliseconds", &context)?;
        if !(offset + duration).is_finite() {
            return Err(format!("Azure Speech returned an invalid end timestamp in {context}."));
        }
        let text = phrase
            .get("text")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| format!("Azure Speech returned missing or invalid text in {context}."))?
            .trim();
        let speaker_id = match phrase.get("speaker") {
            None | Some(serde_json::Value::Null) => None,
            Some(value) => Some(
                value
                    .as_u64()
                    .ok_or_else(|| format!("Azure Speech returned an invalid speaker in {context}."))?
                    .to_string(),
            ),
        };
        if text.is_empty() {
            continue;
        }
        // Only speaker fields are added to the shared segment model. Phrase duration
        // is validated above; phrase locale/duration are not persisted as new fields.
        transcript.push(FastTranscriptionPhrase {
            offset_seconds: offset / 1_000.0,
            text: text.to_string(),
            speaker_id,
        });
    }
    Ok(transcript)
}

fn fast_transcription_duration(
    result: &serde_json::Value,
    source_duration: Duration,
) -> Result<f64, String> {
    if result.get("durationMilliseconds").is_none() {
        return Ok(source_duration.as_secs_f64());
    }
    Ok(fast_transcription_milliseconds(result, "durationMilliseconds", "response")? / 1_000.0)
}

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

// Existing flat IPC args are retained; every upload now also requires explicit consent.
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
    diarization: Option<bool>,
    upload_consent: Option<bool>,
) -> Result<Meeting, String> {
    let definition = fast_transcription_definition(&language, diarization, upload_consent)?;
    let endpoint = fast_transcription_endpoint(&endpoint)?;
    let locale = language.trim().to_string();
    let mut source = PathBuf::from(path);
    let _ = app.emit(
        "fast-transcription-progress",
        serde_json::json!({
            "phase": "validating", "detail": "Validating the selected media file."
        }),
    );
    let (mut extension, _, mut source_duration) = fast_transcription_file_details(&source)?;
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
    let form = reqwest::multipart::Form::new()
        .part(
            "audio",
            reqwest::multipart::Part::bytes(audio)
                .file_name(file_name)
                .mime_str(mime_type)
                .map_err(|error| error.to_string())?,
        )
        .text("definition", definition.to_string());
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
    let transcript = parse_fast_transcription_phrases(&result)?
        .into_iter()
        .map(|phrase| TranscriptSegment {
            id: format!("segment-{}", Uuid::new_v4()),
            offset_seconds: phrase.offset_seconds,
            text: phrase.text,
            speaker_id: phrase.speaker_id,
            // Diarization produces anonymous IDs, never real names.
            speaker_name: None,
        })
        .collect::<Vec<_>>();
    let duration_seconds = fast_transcription_duration(&result, source_duration)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    fn phrase() -> Value {
        json!({
            "offsetMilliseconds": 1250,
            "durationMilliseconds": 500,
            "text": "  Hello  ",
            "locale": "en-US"
        })
    }

    #[test]
    fn upload_requires_explicit_consent_even_without_diarization() {
        for consent in [None, Some(false)] {
            for diarization in [None, Some(false), Some(true)] {
                // Consent takes precedence even over an invalid language.
                let error = fast_transcription_definition("", diarization, consent).unwrap_err();
                assert!(error.contains("Explicit consent"));
                assert!(error.contains("charges"));
            }
        }
    }

    #[test]
    fn definition_omits_diarization_unless_requested() {
        for diarization in [None, Some(false)] {
            assert_eq!(
                fast_transcription_definition(" en-US ", diarization, Some(true)).unwrap(),
                json!({ "locales": ["en-US"] })
            );
        }
        assert_eq!(
            fast_transcription_definition("ko-KR", Some(true), Some(true)).unwrap(),
            json!({ "locales": ["ko-KR"], "diarization": { "enabled": true } })
        );
    }

    #[test]
    fn request_rejects_blank_or_auto_locale() {
        for language in ["", " \t", "auto", " Auto ", "AUTO"] {
            assert!(fast_transcription_definition(language, None, Some(true)).is_err());
        }
    }

    #[test]
    fn parses_zero_and_repeated_speaker_ids_without_inventing_names() {
        let mut first = phrase();
        first["speaker"] = json!(0);
        first["offsetMilliseconds"] = json!(0);
        let mut second = phrase();
        second["speaker"] = json!(2);
        let mut third = phrase();
        third["speaker"] = json!(0);
        third["offsetMilliseconds"] = json!(2500);
        let parsed = parse_fast_transcription_phrases(&json!({
            "phrases": [first, second, third]
        }))
        .unwrap();
        assert_eq!(parsed[0].offset_seconds, 0.0);
        assert_eq!(parsed[1].offset_seconds, 1.25);
        assert_eq!(parsed[2].offset_seconds, 2.5);
        assert_eq!(parsed[0].text, "Hello");
        assert_eq!(parsed[0].speaker_id.as_deref(), Some("0"));
        assert_eq!(parsed[1].speaker_id.as_deref(), Some("2"));
        assert_eq!(parsed[2].speaker_id, parsed[0].speaker_id);
    }

    #[test]
    fn absent_or_null_speaker_remains_unknown() {
        let mut null_speaker = phrase();
        null_speaker["speaker"] = Value::Null;
        let parsed = parse_fast_transcription_phrases(&json!({
            "phrases": [phrase(), null_speaker]
        }))
        .unwrap();
        assert!(parsed.iter().all(|phrase| phrase.speaker_id.is_none()));
    }

    #[test]
    fn invalid_speaker_is_not_silently_discarded() {
        for value in [json!(-1), json!(0.5), json!("0"), json!(true), json!({})] {
            let mut invalid = phrase();
            invalid["speaker"] = value;
            let error = parse_fast_transcription_phrases(&json!({ "phrases": [invalid] }))
                .unwrap_err();
            assert!(error.contains("speaker"));
        }
    }

    #[test]
    fn missing_or_invalid_phrase_timestamps_fail_the_whole_response() {
        for field in ["offsetMilliseconds", "durationMilliseconds"] {
            let mut missing = phrase();
            missing.as_object_mut().unwrap().remove(field);
            assert!(parse_fast_transcription_phrases(&json!({
                "phrases": [phrase(), missing]
            }))
            .unwrap_err()
            .contains(field));
            for value in [Value::Null, json!(-1), json!("1000"), json!(true), json!({})] {
                let mut invalid = phrase();
                invalid[field] = value;
                assert!(parse_fast_transcription_phrases(&json!({ "phrases": [invalid] }))
                    .unwrap_err()
                    .contains(field));
            }
        }
        let mut overflow = phrase();
        overflow["offsetMilliseconds"] = json!(f64::MAX);
        overflow["durationMilliseconds"] = json!(f64::MAX);
        assert!(parse_fast_transcription_phrases(&json!({ "phrases": [overflow] })).is_err());
    }

    #[test]
    fn preserves_fractional_offsets_and_response_order() {
        let mut earlier = phrase();
        earlier["offsetMilliseconds"] = json!(250.5);
        let parsed = parse_fast_transcription_phrases(&json!({
            "phrases": [phrase(), earlier]
        }))
        .unwrap();
        assert_eq!(parsed[0].offset_seconds, 1.25);
        assert_eq!(parsed[1].offset_seconds, 0.2505);
    }

    #[test]
    fn requires_phrase_array_but_accepts_empty_transcription() {
        for response in [
            Value::Null,
            json!({}),
            json!({ "phrases": null }),
            json!({ "phrases": {} }),
            json!({ "combinedPhrases": [{ "text": "No timestamps" }] }),
        ] {
            assert!(parse_fast_transcription_phrases(&response).is_err());
        }
        assert!(parse_fast_transcription_phrases(&json!({ "phrases": [] }))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn skips_only_valid_blank_phrases_and_rejects_malformed_text() {
        let mut blank = phrase();
        blank["text"] = json!(" \n\t ");
        assert!(parse_fast_transcription_phrases(&json!({ "phrases": [blank.clone()] }))
            .unwrap()
            .is_empty());
        blank.as_object_mut().unwrap().remove("offsetMilliseconds");
        assert!(parse_fast_transcription_phrases(&json!({ "phrases": [blank] })).is_err());
        for text in [Value::Null, json!(123), json!({})] {
            let mut invalid = phrase();
            invalid["text"] = text;
            assert!(parse_fast_transcription_phrases(&json!({ "phrases": [invalid] })).is_err());
        }
    }

    #[test]
    fn response_duration_falls_back_only_when_absent() {
        let local = Duration::from_millis(5000);
        assert_eq!(fast_transcription_duration(&json!({}), local).unwrap(), 5.0);
        assert_eq!(
            fast_transcription_duration(&json!({ "durationMilliseconds": 1250 }), local).unwrap(),
            1.25
        );
        for value in [Value::Null, json!(-1), json!("5000"), json!(true)] {
            assert!(fast_transcription_duration(&json!({ "durationMilliseconds": value }), local)
                .is_err());
        }
    }
}

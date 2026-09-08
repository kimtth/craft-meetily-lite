use crate::{write_store, AppState, Meeting};

fn rename(meeting: &mut Meeting, speaker_id: &str, name: &str) -> Result<(), String> {
    let name = name.trim();
    if name.chars().count() > 100 || name.chars().any(char::is_control) {
        return Err("Speaker name must be at most 100 characters without control characters".into());
    }
    if !meeting.transcript.iter().any(|s| s.speaker_id.as_deref() == Some(speaker_id)) {
        return Err("Speaker not found in this meeting".into());
    }
    for segment in &mut meeting.transcript {
        if segment.speaker_id.as_deref() == Some(speaker_id) {
            segment.speaker_name = (!name.is_empty()).then(|| name.to_owned());
        }
    }
    meeting.updated_at = chrono::Utc::now().to_rfc3339();
    Ok(())
}

#[tauri::command]
pub(crate) fn rename_speaker(
    state: tauri::State<'_, AppState>, meeting_id: String, speaker_id: String, name: String,
) -> Result<Meeting, String> {
    let mut store = state.store.lock().map_err(|_| "Meeting store unavailable")?;
    let index = store.meetings.iter().position(|m| m.id == meeting_id).ok_or("Meeting not found")?;
    let previous = store.meetings[index].clone();
    rename(&mut store.meetings[index], &speaker_id, &name)?;
    if let Err(error) = write_store(&state.data_dir, &store) {
        store.meetings[index] = previous;
        return Err(error.to_string());
    }
    Ok(store.meetings[index].clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn meeting() -> Meeting {
        serde_json::from_value(serde_json::json!({"id":"m", "title":"test", "createdAt":"", "updatedAt":"",
            "durationSeconds":2, "hasAudio":false, "recordingPath":null,
            "transcript":[{"id":"a","offsetSeconds":0,"text":"one","speakerId":"0"},
                {"id":"b","offsetSeconds":1,"text":"two","speakerId":"1"},
                {"id":"c","offsetSeconds":2,"text":"three","speakerId":"0"}]})).unwrap()
    }
    #[test]
    fn names_are_meeting_scoped_and_clearable() {
        let mut m = meeting();
        rename(&mut m, "0", "  Alice  ").unwrap();
        assert_eq!(m.transcript[0].speaker_name.as_deref(), Some("Alice"));
        assert_eq!(m.transcript[2].speaker_name.as_deref(), Some("Alice"));
        assert!(m.transcript[1].speaker_name.is_none());
        rename(&mut m, "0", "").unwrap();
        assert!(m.transcript[0].speaker_name.is_none());
        assert!(rename(&mut m, "missing", "Name").is_err());
        assert!(rename(&mut m, "0", "bad\nname").is_err());
    }
    #[test]
    fn legacy_transcript_needs_no_speaker_migration() {
        let segment: crate::TranscriptSegment = serde_json::from_str(r#"{"id":"old","offsetSeconds":1,"text":"legacy"}"#).unwrap();
        assert!(segment.speaker_id.is_none());
    }
}
//! Assistant-only storage. Never reads/writes the application's store file.
use super::schema::{self, AssistantMessage, AssistantState, Kind, ModelReply, Role, Snapshot};
use crate::Meeting;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs::{self, File, OpenOptions}, io::{Read, Write}, path::{Path, PathBuf}};
use uuid::Uuid;

const MAX_FILE_BYTES: usize = 2_000_000;

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Saved {
    version: u32,
    meeting_id: String,
    pub assistant: AssistantState,
    pub completions: BTreeMap<String, bool>,
    context_start: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    context_snapshot: Option<Snapshot>,
}

impl Saved {
    fn context_is_safe(&self, meeting: &Meeting) -> Result<bool, String> {
        if let Some(snapshot) = &self.context_snapshot {
            snapshot.is_safe_for(meeting)
        } else {
            // Legacy files have no prefix proof: exact equality only. An append
            // omits legacy context safely, but never blocks a fresh question.
            Ok(self.assistant.transcript_fingerprint.as_deref() == Some(&schema::fingerprint(meeting)?))
        }
    }

    pub(super) fn state_for(&self, meeting: &Meeting) -> Result<AssistantState, String> {
        let mut result = self.assistant.clone();
        // Non-null means the covered context is still safe, NOT that it covers
        // all current speech. Preserve the original fingerprint and coverage
        // after append; never relabel an old answer as covering newer evidence.
        if !self.context_is_safe(meeting)? { result.transcript_fingerprint = None; }
        Ok(result)
    }

    pub(super) fn history_for(&self, meeting: &Meeting) -> Result<Vec<AssistantMessage>, String> {
        Ok(if self.context_is_safe(meeting)? {
            self.assistant.messages[self.context_start..].to_vec()
        } else { Vec::new() })
    }

    pub(super) fn apply_reply(&mut self, meeting: &Meeting, kind: Kind, prompt: String, reply: ModelReply) -> Result<(), String> {
        let snapshot = Snapshot::capture(meeting)?;
        if !self.context_is_safe(meeting)? {
            self.context_start = self.assistant.messages.len();
        }
        let (answer, ids) = match reply {
            ModelReply::Recap { recap, action_items } => {
                // Preserve previously completed tasks even if a recap temporarily
                // omits them. Matching is exact normalized task/owner/due, not fuzzy.
                for a in &self.assistant.action_items { self.completions.insert(a.id.clone(), a.done); }
                self.assistant.action_items = schema::actions(&meeting.id, action_items, &self.completions);
                let message = (recap.summary.clone(), recap.source_ids.clone());
                self.assistant.recap = Some(recap);
                message
            }
            ModelReply::Chat { text, source_ids } => (text, source_ids),
        };
        if self.completions.len() > 1_000 { return Err("Action completion history limit reached; no data was changed".into()); }
        self.assistant.messages.push(AssistantMessage {
            id: Uuid::new_v4().to_string(), role: Role::User,
            text: if kind == Kind::Recap && prompt.trim().is_empty() { "Generate meeting recap".into() } else { prompt },
            source_ids: Vec::new(),
            context_through_seconds: Some(snapshot.through_seconds),
            context_segment_count: Some(snapshot.segment_count),
        });
        self.assistant.messages.push(AssistantMessage {
            id: Uuid::new_v4().to_string(), role: Role::Assistant, text: answer, source_ids: ids,
            context_through_seconds: Some(snapshot.through_seconds),
            context_segment_count: Some(snapshot.segment_count),
        });
        self.assistant.transcript_fingerprint = Some(snapshot.transcript_fingerprint.clone());
        self.assistant.context_through_seconds = Some(snapshot.through_seconds);
        self.assistant.context_segment_count = Some(snapshot.segment_count);
        self.context_snapshot = Some(snapshot);
        Ok(())
    }
}

fn path(root: &Path, meeting_id: &str) -> PathBuf {
    root.join("assistant").join(format!("{}.json", schema::hash(meeting_id.as_bytes())))
}

pub(super) fn remove(root: &Path, meeting_id: &str) -> Result<(), String> {
    match fs::remove_file(path(root, meeting_id)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err("Could not delete private assistant data; meeting deletion stopped".into()),
    }
}

pub(super) fn lock(root: &Path, meeting_id: &str) -> Result<File, String> {
    // All callers must first verify the meeting exists. OS locks reject concurrent
    // requests, including a second app process. The file guard IS held across
    // inference awaits; the application's Store mutex is not. Recording appends
    // use Store, not this lock, and remain free to proceed.
    let path = path(root, meeting_id);
    fs::create_dir_all(path.parent().ok_or("Invalid assistant path")?)
        .map_err(|_| "Could not create assistant directory")?;
    let file = OpenOptions::new().create(true).truncate(false).read(true).write(true)
        .open(path.with_extension("lock")).map_err(|_| "Could not open assistant lock")?;
    file.try_lock_exclusive().map_err(|_| "This meeting's assistant is busy; retry after the current operation finishes")?;
    // Never delete the lock path: unlinking a locked inode would permit races.
    Ok(file)
}

pub(super) fn load(root: &Path, meeting_id: &str) -> Result<Saved, String> {
    let file = match File::open(path(root, meeting_id)) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Saved {
            version: 1, meeting_id: meeting_id.into(), ..Saved::default()
        }),
        Err(_) => return Err("Could not read assistant data".into()),
    };
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES as u64 + 1).read_to_end(&mut bytes).map_err(|_| "Could not read assistant data")?;
    decode(&bytes, meeting_id)
}

fn decode(bytes: &[u8], meeting_id: &str) -> Result<Saved, String> {
    if bytes.len() > MAX_FILE_BYTES { return Err("Assistant file exceeds the storage budget".into()); }
    let saved: Saved = serde_json::from_slice(bytes)
        .map_err(|_| "Assistant data is invalid; existing data was not overwritten")?;
    if saved.version != 1 || saved.meeting_id != meeting_id
        || saved.context_start > saved.assistant.messages.len()
        || saved.assistant.messages.len() > 200 || saved.completions.len() > 1_000
    { return Err("Assistant data has an unsupported version, identity or history size".into()); }
    let valid_coverage = |count: Option<usize>, through: Option<f64>| match (count, through) {
        (None, None) => true,
        (Some(count), Some(through)) => (1..=10_000).contains(&count) && through.is_finite() && through >= 0.0,
        _ => false,
    };
    if !valid_coverage(saved.assistant.context_segment_count, saved.assistant.context_through_seconds)
        || saved.assistant.messages.iter().any(|m| !valid_coverage(m.context_segment_count, m.context_through_seconds)) {
        return Err("Assistant snapshot coverage is invalid; existing data was not overwritten".into());
    }
    if let Some(snapshot) = &saved.context_snapshot {
        if !snapshot.is_well_formed()
            || saved.assistant.transcript_fingerprint.as_deref() != Some(snapshot.transcript_fingerprint.as_str())
            || saved.assistant.context_segment_count != Some(snapshot.segment_count)
            || saved.assistant.context_through_seconds != Some(snapshot.through_seconds) {
            return Err("Assistant snapshot integrity metadata is invalid; existing data was not overwritten".into());
        }
    } else if saved.assistant.context_segment_count.is_some() {
        return Err("Assistant snapshot proof is missing; existing data was not overwritten".into());
    }
    Ok(saved)
}

pub(super) fn save(root: &Path, meeting_id: &str, saved: &Saved) -> Result<(), String> {
    if saved.meeting_id != meeting_id { return Err("Assistant meeting identity mismatch".into()); }
    let bytes = serde_json::to_vec(saved).map_err(|_| "Could not encode assistant data")?;
    decode(&bytes, meeting_id)?;
    let path = path(root, meeting_id);
    let dir = path.parent().ok_or("Invalid assistant path")?;
    let mut temp = tempfile::NamedTempFile::new_in(dir).map_err(|_| "Could not create assistant temporary file")?;
    temp.write_all(&bytes).and_then(|_| temp.as_file().sync_all())
        .map_err(|_| "Could not write assistant data; previous data is unchanged")?;
    // Same-directory atomic replacement, including an EXISTING Windows target.
    // Never delete-then-rename; tempfile handles the platform replacement API.
    temp.persist(&path).map_err(|_| "Could not replace assistant data; previous data is unchanged")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use schema::ActionItem;

    #[test]
    fn hashes_keep_paths_bounded_and_meeting_scoped() {
        let root = Path::new("data");
        let malicious = path(root, "../../CON:secret\\file");
        assert_eq!(malicious.parent().unwrap(), root.join("assistant"));
        assert_eq!(malicious.file_stem().unwrap().to_str().unwrap().len(), 64);
        assert_ne!(path(root, "meeting-a"), path(root, "meeting-b"));
    }
    #[test]
    fn storage_identity_prevents_cross_meeting_loads() {
        let saved = Saved { version: 1, meeting_id: "a".into(), ..Saved::default() };
        let bytes = serde_json::to_vec(&saved).unwrap();
        assert!(decode(&bytes, "a").is_ok());
        assert!(decode(&bytes, "b").is_err());
        assert!(decode(b"broken", "a").is_err());
    }
    #[test]
    fn chat_preserves_actions_and_stale_history_is_not_sent() {
        let mut saved = Saved { version: 1, meeting_id: "a".into(), ..Saved::default() };
        let mut meeting = schema::synthetic_meeting();
        meeting.id = "a".into();
        saved.assistant.action_items.push(ActionItem { id: "x".into(), text: "Work".into(),
            owner: None, due: None, done: true, source_ids: vec!["s".into()] });
        let before = serde_json::to_value(&saved.assistant.action_items).unwrap();
        saved.apply_reply(&meeting, Kind::Chat, "Question".into(),
            ModelReply::Chat { text: "Answer".into(), source_ids: vec!["s".into()] }).unwrap();
        assert_eq!(before, serde_json::to_value(&saved.assistant.action_items).unwrap());
        assert_eq!(saved.history_for(&meeting).unwrap().len(), 2);
        meeting.transcript[0].text = "Changed evidence".into();
        assert!(saved.history_for(&meeting).unwrap().is_empty());
        assert_eq!(saved.assistant.messages.len(), 2);
    }

    fn append(meeting: &mut Meeting) {
        let mut next = meeting.transcript.last().unwrap().clone();
        next.id = format!("source-{}", meeting.transcript.len() + 1);
        next.offset_seconds += 25.0;
        next.text = "New confirmed fact".into();
        meeting.transcript.push(next);
    }

    fn answer(saved: &mut Saved, meeting: &Meeting) {
        saved.apply_reply(meeting, Kind::Chat, "Follow-up question".into(),
            ModelReply::Chat { text: "Synthetic answer".into(), source_ids: vec!["source-a".into()] }).unwrap();
    }

    #[test]
    fn append_followups_keep_context_and_roundtrip_historical_coverage() {
        let mut meeting = schema::synthetic_meeting();
        let root = tempfile::tempdir().unwrap();
        let _guard = lock(root.path(), &meeting.id).unwrap();
        let mut saved = load(root.path(), &meeting.id).unwrap();
        answer(&mut saved, &meeting);
        let original = serde_json::to_value(&saved.assistant).unwrap();
        let fingerprint = saved.assistant.transcript_fingerprint.clone();
        append(&mut meeting);
        assert_eq!(saved.history_for(&meeting).unwrap().len(), 2);
        let projected = saved.state_for(&meeting).unwrap();
        assert_eq!(serde_json::to_value(&projected).unwrap(), original);
        assert_eq!(projected.transcript_fingerprint, fingerprint);
        assert_eq!(projected.context_segment_count, Some(2));
        assert_eq!(projected.context_through_seconds, Some(20.0));

        // A question, not a forced recap, incorporates the appended evidence.
        answer(&mut saved, &meeting);
        assert_eq!(saved.context_start, 0);
        assert_eq!(saved.history_for(&meeting).unwrap().len(), 4);
        assert_eq!(saved.assistant.context_segment_count, Some(3));
        assert_eq!(saved.assistant.context_through_seconds, Some(45.0));
        for m in &saved.assistant.messages[..2] {
            assert_eq!(m.context_segment_count, Some(2));
            assert_eq!(m.context_through_seconds, Some(20.0));
        }
        for m in &saved.assistant.messages[2..] {
            assert_eq!(m.context_segment_count, Some(3));
            assert_eq!(m.context_through_seconds, Some(45.0));
        }
        save(root.path(), &meeting.id, &saved).unwrap();
        let disk_before = fs::read(path(root.path(), &meeting.id)).unwrap();
        let reloaded = load(root.path(), &meeting.id).unwrap();
        append(&mut meeting);
        assert_eq!(reloaded.history_for(&meeting).unwrap().len(), 4);
        assert_eq!(reloaded.state_for(&meeting).unwrap().context_segment_count, Some(3));
        assert_eq!(disk_before, fs::read(path(root.path(), &meeting.id)).unwrap());
    }

    #[test]
    fn changed_evidence_omits_context_but_accepts_new_chat_and_preserves_saved_content() {
        let mut meeting = schema::synthetic_meeting();
        let mut saved = Saved { version: 1, meeting_id: meeting.id.clone(), ..Saved::default() };
        answer(&mut saved, &meeting);
        saved.completions.insert("completed-task".into(), true);
        let old_messages = serde_json::to_value(&saved.assistant.messages).unwrap();
        meeting.transcript[0].text = "Corrected fact".into();
        let historical = saved.state_for(&meeting).unwrap();
        assert!(historical.transcript_fingerprint.is_none());
        assert_eq!(historical.context_segment_count, Some(2));
        assert_eq!(serde_json::to_value(historical.messages).unwrap(), old_messages);
        assert!(saved.history_for(&meeting).unwrap().is_empty());
        answer(&mut saved, &meeting);
        assert_eq!(saved.context_start, 2);
        assert_eq!(saved.assistant.messages.len(), 4);
        assert_eq!(saved.history_for(&meeting).unwrap().len(), 2);
        assert_eq!(saved.completions.get("completed-task"), Some(&true));
        append(&mut meeting);
        answer(&mut saved, &meeting);
        assert_eq!(saved.context_start, 2);
        assert_eq!(saved.history_for(&meeting).unwrap().len(), 4);
        assert_eq!(serde_json::to_value(&saved.assistant.messages[..2]).unwrap(), old_messages);
        assert!(saved.state_for(&meeting).unwrap().transcript_fingerprint.is_some());
    }

    #[test]
    fn legacy_v1_without_coverage_loads_and_upgrades_without_inventing_old_coverage() {
        let mut meeting = schema::synthetic_meeting();
        let legacy = serde_json::json!({"version":1, "meetingId":meeting.id,
            "assistant":{"messages":[
                {"id":"old-user", "role":"user", "text":"Question", "sourceIds":[]},
                {"id":"old-answer", "role":"assistant", "text":"Old answer", "sourceIds":["source-a"]}
            ], "actionItems":[], "transcriptFingerprint":schema::fingerprint(&meeting).unwrap()},
            "completions":{"old-task":true}, "contextStart":0});
        let mut saved = decode(&serde_json::to_vec(&legacy).unwrap(), &meeting.id).unwrap();
        assert_eq!(saved.history_for(&meeting).unwrap().len(), 2);
        assert!(saved.assistant.context_segment_count.is_none());
        assert!(saved.context_snapshot.is_none());
        assert!(saved.state_for(&meeting).unwrap().transcript_fingerprint.is_some());
        let encoded = serde_json::to_value(&saved).unwrap();
        assert!(encoded.get("contextSnapshot").is_none());
        assert!(encoded["assistant"].get("contextSegmentCount").is_none());
        // Exact legacy evidence can be upgraded with a fresh server proof.
        let mut exact = decode(&serde_json::to_vec(&legacy).unwrap(), &meeting.id).unwrap();
        answer(&mut exact, &meeting);
        assert_eq!(exact.context_start, 0);
        assert_eq!(exact.history_for(&meeting).unwrap().len(), 4);
        append(&mut meeting);
        assert_eq!(exact.history_for(&meeting).unwrap().len(), 4);
        // Without a proof, an append cannot certify old history. Still allow a
        // new chat; preserve legacy text/completions and leave its coverage absent.
        assert!(saved.history_for(&meeting).unwrap().is_empty());
        assert!(saved.state_for(&meeting).unwrap().transcript_fingerprint.is_none());
        answer(&mut saved, &meeting);
        assert_eq!(saved.context_start, 2);
        assert_eq!(saved.history_for(&meeting).unwrap().len(), 2);
        assert!(saved.assistant.messages[0].context_segment_count.is_none());
        assert!(saved.assistant.messages[1].context_through_seconds.is_none());
        assert_eq!(saved.assistant.messages[2].context_segment_count, Some(3));
        assert_eq!(saved.completions.get("old-task"), Some(&true));
        let reloaded = decode(&serde_json::to_vec(&saved).unwrap(), &meeting.id).unwrap();
        assert!(reloaded.context_snapshot.is_some());
        assert_eq!(reloaded.history_for(&meeting).unwrap().len(), 2);
    }

    #[test]
    fn malformed_snapshot_metadata_is_not_silently_treated_as_legacy() {
        let meeting = schema::synthetic_meeting();
        let mut saved = Saved { version: 1, meeting_id: meeting.id.clone(), ..Saved::default() };
        answer(&mut saved, &meeting);
        let valid = serde_json::to_value(&saved).unwrap();
        for (pointer, bad) in [
            ("/contextSnapshot/segmentCount", serde_json::json!(0)),
            ("/contextSnapshot/integrity", serde_json::json!("bad")),
            ("/contextSnapshot/throughSeconds", serde_json::json!(-1)),
            ("/assistant/contextSegmentCount", serde_json::json!(3)),
            ("/assistant/contextThroughSeconds", serde_json::json!(999)),
            ("/assistant/messages/0/contextThroughSeconds", serde_json::Value::Null),
            ("/contextSnapshot", serde_json::Value::Null),
        ] {
            let mut invalid = valid.clone();
            *invalid.pointer_mut(pointer).unwrap() = bad;
            assert!(decode(&serde_json::to_vec(&invalid).unwrap(), &meeting.id).is_err(), "{pointer}");
        }
    }
}
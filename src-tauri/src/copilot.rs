//! Meeting assistant IPC. This module never writes the application's Store.
//! SDK/runtime/auth packaging validation remains assumption A7; no inference
//! runs except an explicit, consented ask_copilot invocation.
mod schema;
mod runtime;
mod storage;

pub(crate) use schema::{AssistantState, CopilotStatus};
use crate::{AppState, Meeting};
use schema::{Evidence, Kind, ModelReply, Snapshot};
use serde_json::json;
use std::{collections::BTreeSet, future::Future, sync::OnceLock, time::Duration};
use tauri::{AppHandle, State};
use tokio::sync::Semaphore;

// Bound paid work across meetings as well as within one meeting.
static RUNTIME_SLOTS: OnceLock<Semaphore> = OnceLock::new();

// One extra paid turn at most across the entire user request, not per chunk.
// Retry only validation failures using the original evidence and safe validator
// feedback; never feed rejected output back as instructions or persist it.
async fn generate_validated<T, F, Fut, V>(
    mut input: serde_json::Value,
    stage: &str,
    retry_used: &mut bool,
    mut generate: F,
    validate: V,
) -> Result<T, String>
where
    F: FnMut(serde_json::Value) -> Fut,
    Fut: Future<Output = Result<String, String>>,
    V: Fn(&str) -> Result<T, String>,
{
    loop {
        let raw = generate(input.clone()).await.map_err(|error| format!("{stage}: {error}"))?;
        match validate(&raw) {
            Ok(value) => return Ok(value),
            Err(error) if !*retry_used => {
                *retry_used = true;
                input["validationFeedback"] = json!({
                    "error": error,
                    "instruction": "Regenerate from the supplied evidence. Use only exact allowedSourceIds supporting each claim. Do not invent missing evidence. Follow the required JSON shape and byte limits."
                });
            }
            Err(error) => return Err(format!("{stage}: {error}. The request's single validation retry has been used; existing saved results are unchanged.")),
        }
    }
}

pub(crate) fn remove_for_meeting(root: &std::path::Path, id: &str) -> Result<std::fs::File, String> {
    let guard = storage::lock(root, id)?;
    storage::remove(root, id)?;
    Ok(guard)
}

fn meeting(state: &AppState, id: &str) -> Result<Meeting, String> {
    state.store.lock().map_err(|_| "Meeting store unavailable".to_string())?
        .meetings.iter().find(|m| m.id == id).cloned()
        .ok_or_else(|| "Meeting not found".to_string())
}

fn validate_current(current: &Meeting, snapshot: &Snapshot) -> Result<(), String> {
    if !snapshot.is_safe_for(current)? {
        return Err("Snapshot evidence or meeting metadata changed during the request; result discarded without replacing assistant state. Ask again using the current transcript.".into());
    }
    Ok(())
}

fn check_current(state: &AppState, id: &str, snapshot: &Snapshot) -> Result<(), String> {
    validate_current(&meeting(state, id)?, snapshot)
}

// Caller holds the Store mutex across this check and atomic file replacement.
fn commit_snapshot(root: &std::path::Path, latest: &Meeting, snapshot: &Snapshot, saved: &storage::Saved) -> Result<(), String> {
    validate_current(latest, snapshot)?;
    storage::save(root, &latest.id, saved)
}

fn history_input(saved: &storage::Saved, current: &Meeting) -> Result<serde_json::Value, String> {
    // Strip structured citation IDs and coverage; do not send recap/action
    // objects as evidence. Text (including embedded IDs) is context only and
    // cannot extend the model's request-local citation allowlist.
    let history: Vec<_> = saved.history_for(current)?.into_iter()
        .map(|message| json!({"role":message.role, "text":message.text})).collect();
    let history_json = serde_json::to_string(&history).map_err(|_| "Could not encode history")?;
    if history_json.len() > schema::MAX_HISTORY_BYTES || saved.assistant.messages.len() > 198 {
        return Err("Assistant history limit reached; no history was truncated or sent.".into());
    }
    Ok(json!(history))
}

#[tauri::command]
pub(crate) async fn get_meeting_assistant(
    meeting_id: String,
    state: State<'_, AppState>,
) -> Result<AssistantState, String> {
    meeting(&state, &meeting_id)?;
    let _lock = storage::lock(&state.data_dir, &meeting_id)?;
    let saved = storage::load(&state.data_dir, &meeting_id)?;
    let current = meeting(&state, &meeting_id)?;
    // Reading never writes assistant JSON or silently regenerates anything.
    // An append retains the saved response, fingerprint and historical coverage.
    saved.state_for(&current)
}

#[tauri::command]
pub(crate) async fn copilot_status() -> CopilotStatus {
    let Ok(_slot) = RUNTIME_SLOTS.get_or_init(|| Semaphore::new(2)).try_acquire() else {
        return CopilotStatus::unavailable("Copilot is busy; retry status shortly");
    };
    match runtime::Runtime::start().await {
        Ok(runtime) => {
            let result = runtime.status().await;
            match runtime.close().await {
                Ok(()) => result,
                Err(detail) => CopilotStatus::unavailable(detail),
            }
        }
        Err(detail) => CopilotStatus::unavailable(detail),
    }
}

#[tauri::command]
pub(crate) async fn ask_copilot(
    app: AppHandle,
    state: State<'_, AppState>,
    meeting_id: String,
    prompt: String,
    kind: String,
    model: Option<String>,
    consent: bool,
) -> Result<AssistantState, String> {
    // Must remain before runtime extraction/start, auth checks, or prompt construction.
    let kind = schema::validate_request(consent, &kind, &prompt, model.as_deref())?;
    // Server-owned clone of confirmed Store segments at entry. Recording can
    // append freely afterward; no caller-supplied or provisional evidence.
    let current = meeting(&state, &meeting_id)?;
    let snapshot = Snapshot::capture(&current)?;
    let evidence = Evidence::from_meeting(&current)?;
    let (model_evidence, source_map) = evidence.for_model()?;
    let _lock = storage::lock(&state.data_dir, &meeting_id)?;
    let mut saved = storage::load(&state.data_dir, &meeting_id)?;
    let chunks = model_evidence.chunks()?; // Preflight every chunk before cloud work.
    // Safe append-only turns keep context; edits/legacy uncertainty omit it.
    // Neither case requires generating a recap before the next question.
    let history = history_input(&saved, &current)?;
    let _slot = RUNTIME_SLOTS.get_or_init(|| Semaphore::new(2)).try_acquire()
        .map_err(|_| "Copilot is busy with other meetings; retry shortly")?;
    let runtime = runtime::Runtime::start().await?;
    let work = async {
        let status = runtime.status().await;
        if !status.authenticated || status.models.is_empty() {
            return Err(status.detail);
        }
        let chosen = match model {
            Some(ref id) if status.models.iter().any(|m| &m.id == id) => id.clone(),
            Some(_) => return Err("Selected model is unavailable; refresh Copilot status".into()),
            None => status.models[0].id.clone(),
        };
        let mut notes = Vec::new();
        let mut retry_used = false;
        if chunks.len() > 1 {
            for (index, chunk) in chunks.iter().enumerate() {
                check_current(&state, &meeting_id, &snapshot)?;
                let permitted: BTreeSet<_> = chunk.iter().map(|part| part.id.clone()).collect();
                let input = json!({"task":"chunk", "part":index + 1,
                    "parts":chunks.len(), "question":prompt, "transcript":chunk,
                    "allowedSourceIds":permitted});
                let note = generate_validated(input, &format!("Transcript chunk {}/{}", index + 1, chunks.len()),
                    &mut retry_used,
                    |input| runtime.generate(&chosen, schema::CHUNK_INSTRUCTIONS, input),
                    |raw| schema::validate_chunk(raw, chunk)).await?;
                notes.push(note);
            }
        }
        check_current(&state, &meeting_id, &snapshot)?;
        let permitted: BTreeSet<String> = if notes.is_empty() {
            chunks[0].iter().map(|part| part.id.clone()).collect()
        } else {
            notes.iter().flat_map(|note| note.source_ids.iter().cloned()).collect()
        };
        let input = if notes.is_empty() {
            json!({"task":kind.as_str(), "question":prompt,
                "transcript":chunks[0], "history":history, "allowedSourceIds":permitted})
        } else {
            json!({"task":kind.as_str(), "question":prompt,
                "chunkNotes":notes, "history":history, "allowedSourceIds":permitted,
                "notice":"Every transcript chunk was summarized; summaries are lossy. Do not claim exhaustive detail."})
        };
        let mut reply = generate_validated(input, "Final meeting answer", &mut retry_used,
            |input| runtime.generate(&chosen, schema::REPLY_INSTRUCTIONS, input),
            |raw| {
                let reply = schema::validate_reply(raw, kind, &model_evidence)?;
                // Synthesis citations must also have survived summarization.
                schema::validate_reply_sources(&reply, &permitted)?;
                Ok(reply)
            }).await?;
        // UI/storage/playback still receive original segment IDs, never aliases.
        reply.restore_source_ids(&source_map)?;
        Ok::<ModelReply, String>(reply)
    };
    let result = tokio::time::timeout(Duration::from_secs(600), work).await
        .map_err(|_| "Copilot request exceeded the 10-minute budget; nothing was saved".to_string())
        .and_then(|r| r);
    let cleanup = runtime.close().await;
    let reply = result?;
    cleanup?;
    saved.apply_reply(&current, kind, prompt, reply)?;
    // No await while holding Store; check + atomic assistant commit cannot race
    // transcript append/deletion. Store is READ ONLY throughout this module.
    {
        let store = state.store.lock().map_err(|_| "Meeting store unavailable")?;
        let latest = store.meetings.iter().find(|m| m.id == meeting_id)
            .ok_or("Meeting was deleted; result discarded")?;
        commit_snapshot(&state.data_dir, latest, &snapshot, &saved)?;
    }
    // Deliberately no provisional copilot-delta events: only validated committed
    // content is returned. No background subscription/task can outlive the call.
    let _ = app;
    Ok(saved.assistant)
}

#[tauri::command]
pub(crate) async fn set_action_item_done(
    state: State<'_, AppState>,
    meeting_id: String,
    action_id: String,
    done: bool,
) -> Result<AssistantState, String> {
    meeting(&state, &meeting_id)?;
    let _lock = storage::lock(&state.data_dir, &meeting_id)?;
    let mut saved = storage::load(&state.data_dir, &meeting_id)?;
    let action = saved.assistant.action_items.iter_mut().find(|a| a.id == action_id)
        .ok_or("Action item not found in this meeting")?;
    action.done = done;
    saved.completions.insert(action.id.clone(), done);
    let store = state.store.lock().map_err(|_| "Meeting store unavailable")?;
    let current = store.meetings.iter().find(|m| m.id == meeting_id).ok_or("Meeting not found")?;
    storage::save(&state.data_dir, &meeting_id, &saved)?;
    saved.state_for(current)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, collections::VecDeque, future::ready, rc::Rc};

    fn chunk() -> Vec<schema::EvidencePart> {
        vec![schema::EvidencePart { id: "e1".into(), offset_seconds: 42.0, text: "Synthetic fact".into() }]
    }

    #[tokio::test]
    async fn append_during_inference_commits_only_entry_evidence_and_keeps_followup_context() {
        let entry = schema::synthetic_meeting();
        let snapshot = Snapshot::capture(&entry).unwrap();
        let (evidence, aliases) = Evidence::from_meeting(&entry).unwrap().for_model().unwrap();
        let live = Rc::new(RefCell::new(entry.clone()));
        let root = tempfile::tempdir().unwrap();
        let _guard = storage::lock(root.path(), &entry.id).unwrap();
        let mut saved = storage::load(root.path(), &entry.id).unwrap();
        let mut used = false;
        let input = json!({"transcript":evidence.parts, "allowedSourceIds":["e1", "e2"]});
        let inference = generate_validated(input, "Synthetic answer", &mut used, |input| {
            let live = live.clone();
            let root = root.path();
            let id = &entry.id;
            async move {
                tokio::task::yield_now().await;
                assert_eq!(live.borrow().transcript.len(), 3);
                // The assistant file lock still excludes competing writers,
                // while the recording task has freely appended to the meeting.
                assert!(storage::lock(root, id).is_err());
                assert_eq!(input["transcript"].as_array().unwrap().len(), 2);
                assert_eq!(input["allowedSourceIds"], json!(["e1", "e2"]));
                Ok(r#"{"kind":"chat","text":"Synthetic answer","sourceIds":["e1"]}"#.into())
            }
        }, |raw| schema::validate_reply(raw, Kind::Chat, &evidence));
        let recording = async {
            let mut latest = live.borrow_mut();
            let mut appended = latest.transcript[1].clone();
            appended.id = "source-c".into();
            appended.offset_seconds = 40.0;
            appended.text = "A new fact after the question".into();
            latest.transcript.push(appended);
        };
        let (reply, ()) = tokio::join!(inference, recording);
        let mut reply = reply.unwrap();
        reply.restore_source_ids(&aliases).unwrap();
        saved.apply_reply(&entry, Kind::Chat, "Question at entry".into(), reply).unwrap();
        commit_snapshot(root.path(), &live.borrow(), &snapshot, &saved).unwrap();
        let saved = storage::load(root.path(), &entry.id).unwrap();
        let current = live.borrow();
        let state = saved.state_for(&current).unwrap();
        assert_eq!(state.context_segment_count, Some(2));
        assert_eq!(state.context_through_seconds, Some(20.0));
        assert_eq!(state.transcript_fingerprint.as_deref(), Some(snapshot.transcript_fingerprint.as_str()));
        assert_eq!(state.messages[1].source_ids, ["source-a"]);
        // Post-entry speech is neither citation evidence nor an alias in this
        // turn. The next question gets it, alongside context-only old answers.
        assert!(schema::validate_reply(r#"{"kind":"chat","text":"New fact","sourceIds":["e3"]}"#,
            Kind::Chat, &evidence).is_err());
        let history = history_input(&saved, &current).unwrap();
        assert_eq!(history, json!([
            {"role":"user", "text":"Question at entry"},
            {"role":"assistant", "text":"Synthetic answer"}
        ]));
        assert!(!history.to_string().contains("source-a"));
        let (followup, next_aliases) = Evidence::from_meeting(&current).unwrap().for_model().unwrap();
        assert_eq!(followup.parts.len(), 3);
        assert_eq!(next_aliases["e3"], "source-c");
        assert!(schema::validate_reply(r#"{"kind":"chat","text":"New fact","sourceIds":["e3"]}"#,
            Kind::Chat, &followup).is_ok());
    }

    #[tokio::test]
    async fn evidence_mutations_during_inference_fail_commit_without_replacing_saved_state() {
        for mutation in ["edit", "delete", "reorder", "speaker", "metadata"] {
            let entry = schema::synthetic_meeting();
            let snapshot = Snapshot::capture(&entry).unwrap();
            let (evidence, aliases) = Evidence::from_meeting(&entry).unwrap().for_model().unwrap();
            let live = Rc::new(RefCell::new(entry.clone()));
            let root = tempfile::tempdir().unwrap();
            let _guard = storage::lock(root.path(), &entry.id).unwrap();
            let mut saved = storage::load(root.path(), &entry.id).unwrap();
            saved.apply_reply(&entry, Kind::Chat, "Previous question".into(),
                ModelReply::Chat { text: "Previously saved answer".into(), source_ids: vec!["source-a".into()] }).unwrap();
            storage::save(root.path(), &entry.id, &saved).unwrap();
            let before = serde_json::to_vec(&saved).unwrap();
            let inference = async {
                tokio::task::yield_now().await;
                // Valid original citations cannot bypass the final snapshot
                // check when their underlying evidence changed during await.
                schema::validate_reply(r#"{"kind":"chat","text":"New answer","sourceIds":["e1"]}"#,
                    Kind::Chat, &evidence).unwrap()
            };
            let recording = async {
                let mut changed = live.borrow_mut();
                match mutation {
                    "edit" => changed.transcript[0].text.push_str(" changed"),
                    "delete" => { changed.transcript.remove(0); }
                    "reorder" => changed.transcript.swap(0, 1),
                    "speaker" => changed.transcript[0].speaker_name = Some("Someone else".into()),
                    "metadata" => changed.title.push_str(" changed"),
                    _ => unreachable!(),
                }
            };
            let (mut reply, ()) = tokio::join!(inference, recording);
            reply.restore_source_ids(&aliases).unwrap();
            saved.apply_reply(&entry, Kind::Chat, "New question".into(), reply).unwrap();
            assert!(validate_current(&live.borrow(), &snapshot).is_err(), "{mutation}");
            assert!(commit_snapshot(root.path(), &live.borrow(), &snapshot, &saved).is_err(), "{mutation}");
            let reloaded = storage::load(root.path(), &entry.id).unwrap();
            assert_eq!(serde_json::to_vec(&reloaded).unwrap(), before, "{mutation}");
            assert_eq!(history_input(&reloaded, &live.borrow()).unwrap(), json!([]));
        }
    }

    #[tokio::test]
    async fn invalid_citation_retries_once_with_safe_feedback_and_same_evidence() {
        let inputs = RefCell::new(Vec::new());
        let mut outputs = VecDeque::from([
            r#"{"text":"PRIVATE MODEL OUTPUT","sourceIds":["foreign-secret"]}"#,
            r#"{"text":"Synthetic fact","sourceIds":["e1","e1"]}"#,
        ]);
        let input = json!({"transcript":chunk(), "allowedSourceIds":["e1"]});
        let mut used = false;
        let note = generate_validated(input.clone(), "Transcript chunk 1/2", &mut used,
            |input| { inputs.borrow_mut().push(input); ready(Ok(outputs.pop_front().unwrap().to_string())) },
            |raw| schema::validate_chunk(raw, &chunk())).await.unwrap();
        assert!(used);
        assert_eq!(note.source_ids, ["e1"]);
        let inputs = inputs.borrow();
        assert_eq!(inputs.len(), 2);
        assert_eq!(inputs[1]["transcript"], input["transcript"]);
        assert_eq!(inputs[1]["allowedSourceIds"], input["allowedSourceIds"]);
        let feedback = inputs[1]["validationFeedback"].to_string();
        assert!(feedback.contains("chunk.sourceIds"));
        assert!(!feedback.contains("foreign-secret"));
        assert!(!feedback.contains("PRIVATE MODEL OUTPUT"));
    }

    #[tokio::test]
    async fn retry_budget_is_shared_across_chunks_and_final_answer() {
        let mut used = false;
        let mut calls = 0;
        let result = generate_validated(json!({}), "Transcript chunk 2/3", &mut used,
            |_| { calls += 1; ready(Ok(r#"{"text":"Invalid","sourceIds":[]}"#.to_string())) },
            |raw| schema::validate_chunk(raw, &chunk())).await;
        assert_eq!(calls, 2);
        let error = result.unwrap_err();
        assert!(error.contains("Transcript chunk 2/3"));
        assert!(error.contains("missing"));
        assert!(error.contains("existing saved results are unchanged"));
        let mut final_calls = 0;
        let result = generate_validated(json!({}), "Final meeting answer", &mut used,
            |_| { final_calls += 1; ready(Ok("not JSON".to_string())) },
            |raw| schema::validate_chunk(raw, &chunk())).await;
        assert!(result.is_err());
        assert_eq!(final_calls, 1);
    }

    #[tokio::test]
    async fn successful_validation_never_spends_retry_budget() {
        let mut used = false;
        let mut calls = 0;
        generate_validated(json!({}), "Chunk", &mut used,
            |_| { calls += 1; ready(Ok(r#"{"text":"Synthetic","sourceIds":["e1"]}"#.into())) },
            |raw| schema::validate_chunk(raw, &chunk())).await.unwrap();
        assert_eq!(calls, 1);
        assert!(!used);
    }

    #[tokio::test]
    async fn transport_errors_do_not_trigger_paid_validation_retries() {
        let mut used = false;
        let mut calls = 0;
        let error = generate_validated(json!({}), "Final meeting answer", &mut used,
            |_| { calls += 1; ready(Err("Service unavailable".into())) },
            |raw| schema::validate_chunk(raw, &chunk())).await.unwrap_err();
        assert_eq!(calls, 1);
        assert!(!used);
        assert!(error.contains("Final meeting answer: Service unavailable"));
    }
}
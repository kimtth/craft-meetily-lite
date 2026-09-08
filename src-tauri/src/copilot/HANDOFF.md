# Copilot backend handoff

Implementation only; **no terminal commands, dependency fetching, builds, tests,
runtime starts, auth checks or inference were executed by this task**. Editor
diagnostics are not evidence of a successful Rust build. Runtime availability,
authentication, entitlement, packaging and security behavior remain A7 validation
items in the parent-owned assumption register.

### Scope incident requiring parent review

This session also applied out-of-scope edits to the meeting audio command module.
An initial patch was reversed immediately, but a later patch added decoded-duration
helpers and changed append/capture wiring. That file was changing concurrently;
no broad rollback was attempted because it could overwrite the audio agent's work.
The parent/audio owner must reconcile those edits before compilation. This handoff
does not claim that the final workspace diff is limited to Copilot-owned files.

## Parent integration

Declare `mod copilot;` in the parent library and register these four Tauri commands
under `crate::copilot::`. All are `pub(crate) async fn`:

| Command | Exact Rust parameters, in order | Return |
| --- | --- | --- |
| `get_meeting_assistant` | `meeting_id: String, state: State<'_, AppState>` | `Result<AssistantState, String>` |
| `copilot_status` | none | `CopilotStatus` |
| `ask_copilot` | `app: AppHandle, state: State<'_, AppState>, meeting_id: String, prompt: String, kind: String, model: Option<String>, consent: bool` | `Result<AssistantState, String>` |
| `set_action_item_done` | `state: State<'_, AppState>, meeting_id: String, action_id: String, done: bool` | `Result<AssistantState, String>` |

Tauri injects `app` and `state`; frontend keys are `meetingId`, `actionId`, etc.
`kind` accepts only `recap` and `chat`. Recap permits an empty prompt. Errors are
sanitized strings, never raw SDK errors or prompts. No `copilot-delta` events are
emitted: streaming was optional and is deliberately disabled.

## JSON contract

All DTO keys are camelCase:

- `AssistantState`: `messages: AssistantMessage[]`, optional `recap: Recap`,
  `actionItems: ActionItem[]`, `transcriptFingerprint: string | null`.
- `AssistantMessage`: `id: string`, `role: 'user' | 'assistant'`, `text: string`,
  `sourceIds: string[]`.
- `Recap`: `summary: string`, `decisions: {text: string, sourceIds: string[]}[]`,
  `sourceIds: string[]`.
- `ActionItem`: `id: string`, `text: string`, `owner: string | null`,
  `due: string | null`, `done: boolean`, `sourceIds: string[]`.
- `CopilotStatus`: `authenticated: boolean`, `detail: string`,
  `models: {id: string, name: string}[]`.

`sourceIds` are original transcript segment IDs. The frontend should resolve them
only within the selected meeting and navigate to its `offsetSeconds`. Render
model strings as text, not raw HTML. Historical messages are NOT current evidence.

`transcriptFingerprint: null` means no current validated cache (including a stale
cache); retained recap/history/tasks must be visibly marked historical. A changed
transcript requires an explicit recap before follow-up chat resumes. Historical
messages remain on disk and visible but are excluded from the new context. Action
completion toggles are local and remain possible for historical tasks; the returned
fingerprint stays null when stale. The UI must not label this as a refreshed recap.

## Storage and concurrency

Assistant JSON lives under app data `assistant/<SHA-256(meeting ID)>.json`, with a
versioned meeting-identity envelope, context boundary and completion ledger.
Original Store, transcript/audio files and timestamps are never rewritten.
Unknown meetings are rejected before assistant path access. Same-directory
`NamedTempFile::persist` atomically replaces even existing Windows targets after
file sync. No delete-before-rename fallback.

Per-meeting OS file locks reject concurrent operations, including another app
process; lock sentinels deliberately remain. Two runtimes maximum per app process.
The original Store lock is held only for final fingerprint validation plus atomic
commit, never across an await. Each cloud chunk is preceded by another fingerprint
check. Already-sent content cannot be recalled if recording changes afterward.

Recap task IDs hash meeting ID plus normalized task wording/owner/due. Chat never
changes tasks or completion flags. A completion ledger preserves exact-match task
completion through recap regeneration/temporary omission; paraphrased tasks may
receive new IDs. No fuzzy identity matching is attempted.

Meeting deletion is parent-owned and currently leaves inaccessible assistant files
behind. Parent should decide explicit assistant-data deletion/retention policy;
this task does not change the existing deletion command or remove user data.

## Privacy and runtime

- Consent is checked before extraction, directory creation, auth or transport in
  `ask_copilot`. The consent UI must explicitly cover selected transcript, question,
  and that meeting's assistant history being sent to GitHub Copilot.
- `copilot_status` starts the bundled runtime and checks auth/models, but has no
  meeting input and performs no inference. It may contact GitHub for metadata.
- Official SDK `ClientMode::Empty`, explicit bundled managed runtime path and
  `Transport::Stdio`; no browser, shell invocation, custom REST shim or guessed flags.
- Fresh isolated temporary Copilot home and working directory per invocation.
  Empty tool allowlist, deny-all permissions, qualified tool exclusions, no custom
  agents/MCP, no config/instruction/skill/plugin discovery, memory, cross-session
  store retrieval, host Git operations, hooks, extensions or infinite compaction.
  Session system message replaces the coding-agent prompt. Data remains untrusted.
- Built-in agents cannot execute because all tools (including delegation) are
  excluded. This is configuration isolation, **not an OS security sandbox**.
- CLI logs and session telemetry disabled; inherited Node/Copilot/OTel overrides
  removed, OTel/content capture explicitly disabled. The application itself never
  logs meeting content or SDK errors. Parent should avoid verbose SDK wire tracing.
- Existing logged-in-user authentication is requested. An isolated home does not
  copy Copilot credentials; supported GitHub CLI/environment auth availability must
  be tested. No tokens are copied into app settings or returned to the frontend.
- Every inference session is disconnected and deleted; runtime shutdown is awaited
  with force-stop fallback. Cancellation force-stops via Drop. Temp data is removed
  after process termination; cleanup errors fail the request. Crash/power loss or
  OS file locks can leave `meetly-copilot-*` temporary directories containing data.
  Persistence is plaintext under the user's app-data permissions, not encrypted.

## Budgets and limitations

- Transcript: 180,000 UTF-8 text bytes, 10,000 segments, maximum 12 chunks of
  18,000 serialized JSON bytes. Oversized segments split at UTF-8 boundaries with
  original ID/offset retained. No input text is silently dropped.
- Each multi-chunk request uses one call per chunk plus one synthesis (up to 13).
  Chunk notes: 5,000 output bytes, 3,000 text bytes. Final request: 96,000 bytes
  including instructions; output: 16,000 bytes. Context overflow fails explicitly.
- Prompt: 4,000 bytes; current history: 20,000 serialized bytes; total history:
  200 messages; completion ledger: 1,000 entries; persisted envelope: 2 MB.
- Startup/auth/models/session creation: 30-second timeout each; turn: 120 seconds;
  overall inference work: 10 minutes; teardown RPCs: 5 seconds each. Local bundled
  extraction is blocking work on the Tokio blocking pool, not cancellable mid-write.
- Model defaults to the first alphabetically sorted service-advertised ID; explicit
  model selection is recommended. Model listing is not an entitlement guarantee.
- JSON is prompt-constrained then strictly locally validated, not provider-enforced
  JSON Schema. Unknown fields, foreign/missing citations and wrong response kinds
  fail closed. Citation membership does not prove semantic truth. Owner/due require
  literal text in cited original segments but still require human review.
- Chunk summaries are inherently lossy; every chunk is processed, but summaries
  may omit details. Final citations must survive chunk-note citations too.
- Budgets are bytes, not exact model tokens. A smaller-context model may reject a
  request. There is no automatic retry/model fallback or hidden additional charge.
- Output size is checked after SDK final-message receipt; this is not a transport
  frame-size cap or a provider-side generation-token limit.

## Dependencies and verification

Manifest: `github-copilot-sdk = "=1.0.13"`, defaults disabled, `bundled-cli` enabled;
`fs2 = "0.4"`, `sha2 = "0.10"`, `tempfile = "3"`. Application MSRV raised to 1.94
because the official SDK requires Rust 1.94 (user reports 1.95 available).
SDK build scripts download/checksum bundled CLI and managed runtime archives;
packaging is larger than the original app (upstream archives roughly 132–157 MB
per platform for the documented runtime generation). Windows x64/ARM64 runtime
support is documented; actual package contents and installation must be validated.

Official references:
- https://github.com/github/copilot-sdk/blob/main/rust/README.md
- https://docs.rs/github-copilot-sdk/1.0.13/github_copilot_sdk/
- https://crates.io/crates/github-copilot-sdk/1.0.13

Pure unit tests cover consent/requests, Unicode chunk completeness, transcript
bounds, schema/foreign citation rejection, owner evidence, task IDs/completion,
storage identity/path safety, stale history exclusion and SDK isolation config.
Tests are written but **not run**. Parent must fetch/compile, run pure tests, add
Windows atomic-replacement/locking fault tests, and validate runtime packaging,
auth/no-tools behavior and teardown with synthetic data only. Real meeting cloud
tests still need separate explicit user consent.
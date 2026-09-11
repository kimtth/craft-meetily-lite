---
title: Meetly Lite Rust Backend Guide
description: Maintainer guide for Python developers working on the Meetly Lite Rust backend
---

## Maintainer Guide For Python Developers

Meetly Lite is a Windows-first Tauri desktop app. Rust provides native audio
capture, local file I/O, Foundry Local speech recognition, and screen recording.
React provides the UI and the Azure Speech live-recognition client. If you usually
work in Python, use this guide to connect familiar ideas to the actual backend.

The local speech engine is **Foundry Local**.
**Meeting Assistant** is the feature name; GitHub Copilot is its optional cloud
provider. Neither the assistant nor Azure Speech is required for local recording.

Consult [Architecture.md](../Architecture.md) for broader design.

## Backend Module Map

The Rust backend is split into a small root module plus focused implementation
modules:

| Module | Responsibility |
| --- | --- |
| [src-tauri/src/main.rs](../src-tauri/src/main.rs) | Executable entry point; calls `meetly_lite_lib::run()`. |
| [src-tauri/src/lib.rs](../src-tauri/src/lib.rs) | Shared models, `AppState`, JSON store helpers, startup recovery, and `tauri::generate_handler!` registration. |
| [src-tauri/src/commands/mod.rs](../src-tauri/src/commands/mod.rs) | Shared command support, settings, meeting list/deletion, exports, playback, and mini mode. |
| [src-tauri/src/commands/meetings.rs](../src-tauri/src/commands/meetings.rs) | Recording lifecycle, local inference workers, Azure audio events, model preparation commands, language updates, and audio-encoding recovery. |
| [src-tauri/src/audio/mod.rs](../src-tauri/src/audio/mod.rs) | CPAL/WASAPI devices and streams, mono conversion, streaming resampling, aligned mixing, incremental WAV helpers, and playback support. |
| [src-tauri/src/foundry_local.rs](../src-tauri/src/foundry_local.rs) | Foundry Local SDK service, speech-model catalog/cache, execution-provider preparation, and local transcription. |
| [src-tauri/src/speech_gate.rs](../src-tauri/src/speech_gate.rs) | Speech detection and utterance segmentation, using WebRTC voice activity detection (VAD) to avoid treating silence as speech. |
| [src-tauri/src/azure_auth.rs](../src-tauri/src/azure_auth.rs) | Azure CLI login, tenant/subscription validation, and bearer-token acquisition. |
| [src-tauri/src/commands/transcription.rs](../src-tauri/src/commands/transcription.rs) | Azure Fast Transcription file upload, optional diarization, response parsing, and new-meeting persistence. |
| [src-tauri/src/commands/screen.rs](../src-tauri/src/commands/screen.rs) | Desktop/area selection, screen recording, video catalog, runtime status, and incomplete-video recovery. |
| [src-tauri/src/video/capture.rs](../src-tauri/src/video/capture.rs) | FFmpeg capture process and screen dimensions. Other video-module helpers handle encoding and audio muxing. |
| [src-tauri/src/speakers.rs](../src-tauri/src/speakers.rs) | Locally assigned names for anonymous transcript speaker IDs. |
| [src-tauri/src/windows_call_mute.rs](../src-tauri/src/windows_call_mute.rs) | Windows Teams-call mute monitoring; not a Teams transcript or assistant API integration. |
| [src-tauri/src/copilot.rs](../src-tauri/src/copilot.rs) | Meeting Assistant commands, request snapshots, inference orchestration, and validated result commits. |
| [src-tauri/src/copilot/runtime.rs](../src-tauri/src/copilot/runtime.rs) | Bundled GitHub Copilot SDK runtime, credentials, model discovery, isolated sessions, and cleanup. |
| [src-tauri/src/copilot/schema.rs](../src-tauri/src/copilot/schema.rs) | Assistant data contracts, snapshot integrity, evidence IDs, chunking, and response validation. |
| [src-tauri/src/copilot/storage.rs](../src-tauri/src/copilot/storage.rs) | Locked per-meeting assistant storage and safe-history selection. |

Most frontend command wrappers live in [src/nativeClient.ts](../src/nativeClient.ts).
The assistant invokes its commands directly from
[src/assistant/MeetingAssistant.tsx](../src/assistant/MeetingAssistant.tsx).

## What The Backend Implements

* Tauri command handlers marked with `#[tauri::command]`. These are callable from
  React through `invoke(...)`, similar to exposing Python functions through
  FastAPI or Flask. For example, `start_recording` is called from
  [src/nativeClient.ts](../src/nativeClient.ts). Unlike HTTP endpoints, these are
  local desktop IPC commands; the browser-only preview cannot perform native work.
* Local storage through `Store`, serialized as JSON under the app-data directory.
  `read_store` and `write_store` are closest to `json.load` and `json.dump` over
  a small Python dictionary/list structure.
* Meeting records through `Meeting` and `TranscriptSegment`. Think of these as
  Python `@dataclass` models, except the compiler checks every field at build
  time.
* Local recognition through `TranscriptionEngine::FoundryLocal`; the other live
  engine is `TranscriptionEngine::Azure`. The default engine identifier is
  `foundryLocal`. Foundry Local manages models in its SDK service, not in an
  `AppState.whisper` field.
* Audio device listing through `list_audio_input_devices` and
  `list_audio_output_devices`.
* Recording modes for `microphone`, `system`, and `microphoneSystem`. The default
  is simultaneous microphone plus system audio.
* Two local chunking modes: utterance-based segmentation and fixed five-second
  chunks. Accepted text is persisted and emitted as `transcript-segment` events.
* Azure Speech streaming. When Azure is selected, the Rust backend emits 16 kHz
  PCM chunks through `audio-chunk`; the React layer streams them to Azure Speech
  and persists final text through `add_transcript_segment`.
* Incremental WAV capture followed by local MP3 encoding on successful stop.
  Export uses the stored recording's format; do not assume all recordings are WAV.
  Local playback uses `rodio`.
* Separate Azure file transcription and optional Meeting Assistant chat, recap,
  and action-item workflows, described below.

## Python To Rust Mental Model

* `use package::Thing;` is like `from package import Thing`.
* `struct Meeting { ... }` is like a typed Python dataclass. Rust struct literals
  must initialize every field, including `Option<T>` fields, unless a struct-update
  expression supplies them. `#[serde(default)]` affects JSON deserialization,
  not Rust construction; `Option<T>` represents an optional value, not an optional
  field in a struct literal.
* `Option<T>` means a value may be present. `Some(value)` is present, and `None`
  is like Python `None`.
* `Result<T, E>` means success or failure. `Ok(value)` is success, and
  `Err(error)` is failure. The `?` operator returns early on error, similar to
  raising an exception.
* Rust values are owned by one place at a time. Passing `value` can move it;
  passing `&value` borrows it. If Python intuition says, "I only need to read
  this," expect Rust code to use `&T` or `&str`.
* `.clone()` makes an owned copy or increments a reference-counted handle. It is
  common when a value must stay in app state and also move into an async task.
* `Arc<T>` is an atomic reference-counted pointer. It shares ownership across
  tasks or threads when `T` meets Rust's thread-safety requirements. It does not
  itself make arbitrary mutations safe; `Arc::clone` shares the same allocation.
* `Mutex<T>` protects mutable shared state. Call `.lock()` to access the value.
  The lock is released when the guard goes out of scope.
* `Arc<Mutex<T>>` is used for shared capture state, pending audio, and the recording
  language. Keep lock scopes short and avoid holding a standard mutex guard over
  `.await`; copy the needed data before waiting for inference or network I/O.
* `let mut value = ...` means the variable can be reassigned or mutated.
* `match value { ... }` is a structured `if/elif` for enums and patterns.
* `if let Some(value) = maybe_value { ... }` runs a block only when an optional
  value exists.
* Closures such as `move |data, info| { ... }` are like nested functions that can
  capture surrounding values. `move` transfers captured values into the closure.
* `tauri::async_runtime::spawn(async move { ... })` starts a background async
  task, similar to `asyncio.create_task(...)`.
* `drop(value)` explicitly releases a value early. This is useful for locks,
  streams, or files that must close before later work continues.
* `#[derive(Serialize, Deserialize)]` asks Rust to generate JSON serialization
  code. `#[serde(rename_all = "camelCase")]` maps Rust fields such as
  `audio_device_id` to TypeScript fields such as `audioDeviceId`.

## How Recording Flows

1. The frontend calls `list_audio_input_devices` and `list_audio_output_devices`
   so the user can choose microphone and system audio sources.
2. The frontend calls `start_recording` with selected device IDs, capture mode,
   language, and transcription engine.
3. `commands::meetings::start_recording` validates the selected engine and local
  model when needed, creates recording state and an incremental WAV writer, and
  starts the thread that owns the audio streams. Appending to a meeting retains
  its earlier audio and uses a base offset for the new session.
4. CPAL resolves the microphone and WASAPI loopback devices. Capture callbacks
  convert samples to `f32`, downmix to mono, and resample to 16 kHz.
5. `RecordingCapture` timestamps both sources against a common clock.
  `AlignedMixer` fills missing intervals with silence and produces one mixed
  timeline. Muted microphone samples become silence rather than shortening time.
6. The persistence task drains that mixer, appends PCM to the WAV, and publishes
  the same samples to `PendingAudio` for transcription. Recording and recognition
  therefore do not independently mix different versions of the audio.
7. Foundry Local consumes pending audio using the selected chunking mode. Azure
  mode emits `audio-chunk` events; [src/azureSpeech.ts](../src/azureSpeech.ts)
  streams them to Azure and persists final recognition via `add_transcript_segment`.
8. `stop_recording` stops capture first and drains persistence so the final samples
  reach transcription. It then signals transcription to stop, finalizes the WAV,
  and awaits transcription completion. The Azure frontend also waits for
  end-of-input and outstanding final-text saves.
9. Successful finalization encodes MP3 with FFmpeg, updates meeting metadata, and
  removes superseded intermediate files. Appending creates a new merged recording
  rather than overwriting the earlier audio. Failure paths retain recovery media
  where possible and report the failure instead of claiming complete success.

Recording duration comes from saved samples, not merely the UI timer. Azure
recognition offsets are anchored to delivered PCM; restarting recognition must not
reset the meeting timeline. Language changes apply to later recognition, not to
text already saved. A saved meeting's language selector changes metadata only.

### Foundry Local Model And Chunk Lifecycle

`list_foundry_local_models` returns the SDK's speech-model catalog.
`download_foundry_local_model` prepares execution providers (the native components
that run inference on supported hardware) and the selected model, emitting
`foundry-download-progress` events. Model aliases and cache status come from the
SDK; there is no GGML model-path discovery in the current backend.

The Foundry Local service owns a Tokio runtime and SDK manager. Its blocking-facing
operations are dispatched from recording workers using `spawn_blocking` where
needed. A transcription request writes a temporary WAV, creates a model audio
client with the selected language, and accepts text only after empty/no-speech
filtering. Temporary audio is local.

* **Utterance mode:** `StreamingSpeechSegmenter` groups detected speech; a queued
  utterance keeps the language selected when it was queued.
* **Fixed mode:** `run_fixed_foundry_transcription_task` handles five-second chunks
  and the remaining tail on stop.

Local inference is not a promise of zero network activity during setup: catalog,
execution-provider, and model preparation can require downloads. Prepare the model
before relying on offline recording. A transcription backlog error does not mean
that the recording itself stopped being saved.

## Azure File Transcription And Speakers

[src-tauri/src/commands/transcription.rs](../src-tauri/src/commands/transcription.rs)
implements `select_fast_transcription_audio` and `transcribe_fast_audio`.
The batch UI uses Azure Speech Fast Transcription, not local Foundry Local.

1. Validate the selected WAV, MP3, or MP4 and the configured Azure endpoint.
  The app enforces the 250 MB and two-hour limits.
2. Require explicit upload consent. For MP4, extract MP3 locally with FFmpeg
  before sending audio.
3. Acquire an Azure token and upload directly to Azure Speech using `reqwest`
  multipart data. Azure Blob Storage and SAS URLs are not used.
4. Parse timestamped phrases and optional anonymous speaker IDs, retain a local
  audio copy, and save a new meeting marked `azure-fast`.

Retranscribing existing audio creates a separate meeting; it must not overwrite
the original transcript. Diarization separates speakers but does not establish
their real identities. `rename_speaker` assigns names locally, and transcript
exports include those names.

## Meeting Assistant

Open **Meeting Assistant** to chat about a saved or currently recording meeting.
The UI name is provider-neutral; the backend retains `copilot` module/command names
because the implemented provider is GitHub Copilot.

| UI action | Backend command | Effect |
| --- | --- | --- |
| Check GitHub Copilot status | `copilot_status` | Checks authentication and discovers models; may contact GitHub, but sends no transcript and performs no inference. |
| Reload saved state | `get_meeting_assistant` | Reads local chat, recap, action items, and completion flags again; useful after a loading error. Does not refresh models or generate content. |
| Send / Generate | `ask_copilot` | Explicitly authorizes sharing this meeting's transcript snapshot, question, and eligible assistant history with the provider. |
| Mark an action complete | `set_action_item_done` | Saves a completion flag locally without inference. |

### Snapshots During Recording

A **snapshot** is the server-owned copy of confirmed transcript segments at request
entry. It fixes what the answer can cite while recording continues:

* Chat remains available during recording once confirmed text exists. Startup,
  saving, and conflicting operations can temporarily disable submission.
* Each Send includes the current transcript evidence, not only the new segments.
  Longer transcripts are processed in chunks before answer synthesis.
* Speech appended during a request is available on the next submission. It is not
  pushed into the in-flight model session. There is no automatic background upload
  or delta-only transcript synchronization.
* Snapshot integrity checks allow append-only growth but reject changed/deleted
  evidence or changed meeting metadata. Unsafe old history is excluded from new
  requests without deleting the saved conversation.
* Answers record segment count and the maximum included segment **start** time.
  This is coverage, not recording duration or the end of the last spoken sentence.
* Recaps and action items are manually generated snapshots, not live-updating views.
  Audio-source playback is disabled during capture.

The SDK runtime creates isolated inference sessions and disables tools, ambient
repository discovery, and persistent agent memory. Follow-up context comes from
explicitly supplied local history, not an indefinitely running chat session.
Responses are shown only after validation and local commit, not streamed token by
token. Request-local citation IDs are validated and mapped back to transcript IDs;
at most one extra validation retry is allowed per submission. Long meetings can
therefore consume multiple provider requests and additional usage allowance.

### Claim References

A **claim reference** is a timestamp next to an individual supported statement,
so its transcript evidence can be previewed and played without searching a long
source list. New model replies use `[e1, e10]` brackets (Japanese `［］` / `【】`
and comma variants are accepted). Every reference must occur in that text field's
validated `sourceIds`; synthesis is additionally restricted to sources retained
in chunk notes. Unknown or mixed-invalid brackets reject the response rather than
silently dropping evidence.

Before saving a new reply, Rust converts these brackets to `[[cite:v1:1,2]]`:
the numbers address the **saved field's sourceIds list**, not transcript positions.
That list is restored to original transcript IDs with a single exact lookup in
the request-local alias map. The model cannot emit this reserved marker syntax.
Markers and source lists must remain paired; text without markers gets no invented
claim mapping. Existing output limits and the single retry budget still apply.
This validates source identity and placement, not the factual entailment of a claim.

The UI renders timestamp buttons using React text nodes, previews source text on
hover/focus, and sorts references by transcript timestamp. **Sources (N)** expands
the compact response-level list. Missing/ambiguous sources cannot play; startup,
capture and stale-evidence playback restrictions also apply to inline references.
Saved legacy `eN` bracket groups appear as **Reference unavailable**: their old
request-local mapping cannot safely be reconstructed from today's transcript.
Their structured sources remain available separately, without claiming a per-claim
association. Reading/displaying history never rewrites it. New local markers are
omitted from follow-up context without altering saved text.

### Credentials And Model Discovery

[src-tauri/src/copilot/runtime.rs](../src-tauri/src/copilot/runtime.rs) uses the
official Rust SDK and its bundled runtime. Credentials are read in this order:
`COPILOT_GITHUB_TOKEN`, `GH_TOKEN`, `GITHUB_TOKEN`, then the local GitHub CLI
credential via `gh auth token --hostname github.com`. Missing credentials produce
setup guidance; the app does not automatically open a GitHub login browser.
Never log token values or place them in transcripts or questions.

**Account selection matters:** GitHub CLI can hold both personal and work logins.
The default active login need not be the account used in VS Code. Use **Check
status**, then **Account & details → GitHub account for Meetly** to choose a stored
login, and check status again. Meetly reads that account with `gh auth token
--hostname github.com --user <login>` without switching the global GitHub CLI
account. The chosen username (not its token) is saved locally after a successful
check. Changing accounts clears the old model list and blocks Send until checked.

Explicit account selection overrides ambient token variables for this app and
never falls back to another credential. SDK token authentication can omit the
login; a bounded, no-redirect request to GitHub's official `GET /user` endpoint
verifies the token identity. An identity mismatch blocks the request. The same
selected account is used for discovery and inference, with no automatic sends.

Discovery explicitly passes the same credential used for inference to the pinned
SDK 1.0.13 / runtime 1.0.83 `models.list` API. The selector displays all returned
IDs/names; `auto` is labeled **Auto — GitHub selects the model**. It is a routing
choice, not a specific model or proof of manual-model entitlement.

GitHub's `model_picker_enabled` metadata determines whether a model may appear
in the manual picker. A model may have enabled policy yet still be picker-disabled
and absent from the SDK list. An Auto-only result must not be expanded using a
static catalog or hidden models. Model availability can differ between the SDK,
VS Code, and web products. A successful metadata check does not prove inference
access, and changing account selection alone does not override picker restrictions.

## Local Storage And Recovery

`AppState` holds mutex-protected `Store`, recorder, screen-recorder, and playback
state. Tauri resolves the application-data directory; the fallback uses the local
data directory plus MeetlyLite. Do not assume a single hard-coded Windows path.
The store JSON holds meetings, videos, and settings, while recording media lives
separately. Main-store writes currently use `fs::write`, not atomic replacement;
invalid JSON deserialization falls back to a default store. Preserve a backup
before maintenance rather than relying on this as corruption recovery.

Assistant JSON is stored separately under the assistant subdirectory using a hash
of the meeting ID for the filename. File locks and atomic replacement protect
assistant updates. Assistant commands read the meeting store but do not rewrite
it; deleting a meeting also removes its assistant state.

Startup schedules incomplete screen-recording and audio-encoding recovery.
Failures can leave WAV or partial video files that should be inspected, not deleted
blindly. Never repair historical transcript timestamps by guessing offsets.

## Screen Recording

The screen commands record the desktop or a selected fixed area, not an arbitrary
application window. FFmpeg produces H.264/H.265 MP4, with optional microphone/system
audio muxed during finalization. YUV420 dimensions must be even;
`normalize_yuv420_dimensions` adjusts odd target dimensions.

Incomplete video is kept as partial output until post-processing and validation
complete. `rename_video` changes the catalog title, not the media filename.
Transcribing a saved video is a separate, consented Azure file-transcription action.

## Azure CLI Auth In Multi-Tenant Environments

Azure Speech uses bearer tokens from the local Azure CLI account. Multi-tenant
auth is handled in two layers:

* `sign_in_azure_cli` attempts `az logout --only-show-errors` first to clear
  cached CLI accounts before the next interactive login.
* `sign_in_azure_cli` then runs `az login --allow-no-subscriptions`. If a tenant
  is configured, it adds `--tenant <tenant-id>`.
* If a subscription is configured, it runs `az account set --subscription
  <subscription-id>` after login.
* `check_azure_cli_sign_in` calls the CLI to confirm that a token for
  `https://management.azure.com/` can be issued without a prompt.
* `get_azure_cli_access_token` calls `az account get-access-token` with the resource
  `https://cognitiveservices.azure.com/`. The current implementation invokes the
  CLI directly; it does not use an `AzureCliCredential` object.
* `access_token_args` validates identifiers and prefers `--tenant` when both tenant
  and subscription are configured, because this CLI token command rejects the two
  together. Subscription selection still happens during sign-in.
* Windows invocation resolves the CLI batch launcher through `cmd /C az`, adds
  known install directories when needed, and suppresses console-window flashes.
  Blocking login/token operations run outside the async executor's worker tasks.

The best-effort logout clears cached CLI accounts, including state shared with
other local Azure CLI workflows. It helps avoid accidental reuse of a different
tenant's identity, but does not guarantee the browser forgets its own signed-in
account. `AADSTS50020` can indicate that the selected identity is not a member or
guest of the target tenant.

## Where To Make Common Changes

* Add a native API: update [src/nativeClient.ts](../src/nativeClient.ts), implement
  the command in its domain module, and register it in
  [src-tauri/src/lib.rs](../src-tauri/src/lib.rs). Tauri maps camelCase invocation
  arguments to snake_case Rust parameters.
* Change persisted meeting fields: update `Meeting` and
  [src/types.ts](../src/types.ts), plus serialization defaults and compatibility
  tests for existing JSON.
* Change capture alignment or resampling: start in
  [src-tauri/src/audio/mod.rs](../src-tauri/src/audio/mod.rs).
* Change recording start/stop or chunk workers: use
  [src-tauri/src/commands/meetings.rs](../src-tauri/src/commands/meetings.rs).
  Exports and playback commands remain in
  [src-tauri/src/commands/mod.rs](../src-tauri/src/commands/mod.rs).
* Change model discovery/cache or local inference: use
  [src-tauri/src/foundry_local.rs](../src-tauri/src/foundry_local.rs).
* Change Azure authentication: use
  [src-tauri/src/azure_auth.rs](../src-tauri/src/azure_auth.rs). Live SDK timing and
  shutdown also involve [src/azureSpeech.ts](../src/azureSpeech.ts).
* Change assistant request safety: review
  [src-tauri/src/copilot.rs](../src-tauri/src/copilot.rs) together with its schema,
  runtime, and storage modules; preserve the submission-only sharing boundary.

## Maintenance Tips

* Keep CPAL audio callbacks small. Heavy work belongs in the Tokio task, not on
  the audio callback thread.
* Prefer `Result<..., String>` for Tauri commands so frontend errors stay
  readable.
* Treat the [reference project](../ref-meetily/README.md) as read-only.
* Keep local-first behavior intact. Downloads for local-model setup are distinct
  from uploading meeting audio or text. Test cloud inference only with separate
  authorization; ordinary regression tests should use synthetic inputs.
* Do not hold store locks across inference. Preserve capture, persistence, and
  transcription shutdown ordering so final speech is not dropped.

### Build And Validation

Run these commands from the repository root. On this Windows development machine,
use a **Visual Studio 2022 x64 Native Tools/Developer Command Prompt** for Rust
commands; the other installed Visual Studio toolchain can select the wrong linker.

| Purpose | Command |
| --- | --- |
| Type-check and build frontend assets | `npm run build` |
| Mocked audio/Azure lifecycle regression tests | `npm run test:audio` |
| Assistant snapshot/coverage/composer regression tests | `npm run test:assistant` |
| Native-runtime staging tests | `npm run test:foundry-native-stage` |
| Check Rust compilation | `cargo check --manifest-path src-tauri/Cargo.toml` |
| Rust unit tests | `cargo test --manifest-path src-tauri/Cargo.toml --lib` |
| Native development app | `npm run tauri:dev` |
| Release app and Windows installers | `npm run tauri:build` |
| Release app without installers | `npm run tauri:build -- --no-bundle` |

The release script stages Foundry native DLLs, prepares FFmpeg, builds the frontend,
and invokes Tauri. A frontend build alone does not update an existing desktop
executable. Normal release output is under the Tauri target directory; NSIS/MSI
installers are in its release bundle subdirectory. Close the desktop app normally
before replacing its executable, and never interrupt an active recording.

Tests live both beside their modules and in
[src-tauri/src/test/mod.rs](../src-tauri/src/test/mod.rs). The ignored
`local_bundled_runtime_handshake` test is opt-in: it starts the bundled runtime and
reads local credentials but does not check entitlement, list models, or generate
answers. Compilation and synthetic tests do not establish microphone/loopback
quality, offline readiness, Azure recognition quality, or assistant entitlement.

## Rust Syntax To Recognize

* `unwrap_or_else(|| fallback())` returns the value inside an `Option` when it is
  `Some(...)`, or calls the fallback function when it is `None`. Use it when the
  fallback is a little expensive or needs logic.
* `unwrap_or(value)` returns the value inside an `Option` when it is `Some(...)`,
  or returns the provided fallback value when it is `None`. Use it for cheap,
  already-created defaults.
* `as_deref()` converts `Option<String>` into `Option<&str>`. This is common when
  a function only needs to read text and should not take ownership of the string.
* `as_ref()` converts `Option<T>` into `Option<&T>`. This lets code inspect an
  optional value without moving it out of the owner.
* `samples: &[u16]` means `samples` is a borrowed slice of unsigned 16-bit
  integers. Think of it as a read-only view over part or all of a Python list,
  but without copying the data.
* `&value` borrows a value for reading, and `&mut value` borrows it for mutation.
  Only one mutable borrow can exist at a time.
* `value.to_string()` creates an owned `String` from borrowed text such as `&str`.
  This is similar to copying a Python string when a longer-lived owner needs it.
* `value.clone()` creates another owned handle or copy. In this backend it often
  appears before moving values into audio callbacks or async tasks.
* `map(|value| ...)` transforms an `Option`, `Result`, or iterator item. It is
  similar to applying a small Python lambda when a value exists.
* `map_err(|error| error.to_string())` transforms only the error side of a
  `Result`. Tauri commands use it to convert rich Rust errors into frontend-safe
  `String` messages.
* `ok_or_else(|| "message".to_string())?` turns an `Option` into a `Result` and
  returns early when the option is `None`.
* `?` after a `Result` returns the success value or exits the current function
  with the error. It is the Rust equivalent of "raise this error upward".
* `let Some(value) = maybe_value else { ... };` handles the `None` case first and
  keeps `value` available afterward. It is useful for early returns.
* `match value { ... }` must cover every possible case; a non-exhaustive match is
  a compile error, not just a warning. A wildcard can cover remaining cases.
* `_ => fallback` is the default branch in a `match`, similar to `else`.
* `move || { ... }` or `move |data| { ... }` creates a closure that takes
  ownership of captured values. Audio callbacks and spawned tasks usually need
  `move` because they outlive the function that created them.
* `async move { ... }` creates an async block that owns captured values. This is
  used for background transcription work.
* `tokio::select! { ... }` waits for whichever async event finishes first. The
  recording task uses it to process timer ticks while still responding to stop
  signals.
* `std::mem::take(&mut value)` replaces a value with its default and returns the
  old value. The recorder uses it to drain pending audio buffers without keeping
  stale samples.
* `drop(value)` releases a value immediately instead of waiting until the end of
  the scope. This is used for locks, streams, and files that must close before
  the next operation.
* `clamp(min, max)` limits a number to a range. Audio samples are clamped to
  avoid writing invalid PCM values.
* `pub` makes an item visible outside its module. Use it only when another module
  or crate needs that item.
* `pub(crate)` makes an item visible anywhere inside this Rust crate but not to
  external crates. Most shared backend helpers should use this instead of `pub`.
* `mod audio;` declares the audio module; this project defines it in
  [src-tauri/src/audio/mod.rs](../src-tauri/src/audio/mod.rs).
* `use crate::audio::mix_sources;` imports an item from another module in this
  crate. `crate::` means "start from this Rust package root."
* `#[tauri::command]` marks a function as callable from the frontend through
  Tauri `invoke(...)`.
* `tauri::generate_handler![...]` registers the command functions that the
  frontend is allowed to call.
* `#[derive(Debug, Clone, Serialize, Deserialize)]` asks Rust to generate common
  behavior: debug printing, cloning, JSON serialization, and JSON deserialization.
* `#[serde(rename_all = "camelCase")]` maps Rust snake_case field names to
  TypeScript camelCase field names during JSON serialization.
* `#[serde(default)]` lets old stored JSON load when a newly added field is
  missing. Use it for new `Meeting` fields that should be backward compatible.

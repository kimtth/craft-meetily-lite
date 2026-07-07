---
title: Meetly Lite Rust Backend Guide
description: Maintainer guide for Python developers working on the Meetly Lite Rust backend
---

## Maintainer Guide For Python Developers

Meetly Lite uses a Rust backend because Tauri, local audio capture, local file
I/O, and Whisper model execution need native desktop access. If you usually work
in Python, read this guide as the map from familiar Python ideas to the Rust
modules in `src-tauri/src/`.

## Backend Module Map

The Rust backend is split into a small root module plus focused implementation
modules:

* `src-tauri/src/lib.rs` holds shared data models, app state, store/model helper
  functions, and the Tauri command registration list.
* `src-tauri/src/audio/mod.rs` owns audio mechanics: device listing, CPAL stream
  creation, sample conversion, 16 kHz resampling, microphone/system mixing, WAV
  writing, and local playback setup.
* `src-tauri/src/commands/mod.rs` owns Tauri command handlers. These functions
  are the native API called by `src/nativeClient.ts`.
* `src-tauri/src/azure_auth.rs` owns Azure CLI authentication. It shells out to
  `az` for interactive login/account switching and runs
  `az account get-access-token` to issue bearer tokens for Azure Speech.
* `src-tauri/src/main.rs` is the executable entry point. It calls
  `meetly_lite_lib::run()`.

## What The Backend Implements

* Tauri command handlers marked with `#[tauri::command]`. These are callable from
  React through `invoke(...)`, similar to exposing Python functions through
  FastAPI or Flask. For example, `start_recording` is called from
  `src/nativeClient.ts`.
* Local storage through `Store`, serialized as JSON under the app-data directory.
  `read_store` and `write_store` are closest to `json.load` and `json.dump` over
  a small Python dictionary/list structure.
* Meeting records through `Meeting` and `TranscriptSegment`. Think of these as
  Python `@dataclass` models, except the compiler checks every field at build
  time.
* Whisper model loading through `load_whisper_model`. The model is stored in
  `AppState.whisper` and reused across recordings.
* Audio device listing through `list_audio_input_devices` and
  `list_audio_output_devices`.
* Recording modes for `microphone`, `system`, and `microphoneSystem`. The default
  is simultaneous microphone plus system audio.
* Live local transcription. A Tokio task wakes every 5 seconds, drains pending
  audio, transcribes it with Whisper, saves the transcript segment, and emits a
  `transcript-segment` event.
* Azure Speech streaming. When Azure is selected, the Rust backend emits 16 kHz
  PCM chunks through `audio-chunk`; the React layer streams them to Azure Speech
  and persists final text through `add_transcript_segment`.
* Transcript export as `.txt`, mixed recording export as `.wav`, and local WAV
  playback through `rodio`.

## Python To Rust Mental Model

* `use package::Thing;` is like `from package import Thing`.
* `struct Meeting { ... }` is like a typed Python dataclass. Field names and
  types are fixed. Missing fields are compile errors unless the field uses
  `Option<T>` or `#[serde(default)]`.
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
* `Arc<T>` is an atomic reference-counted pointer. It lets multiple threads or
  tasks share the same value.
* `Mutex<T>` protects mutable shared state. Call `.lock()` to access the value.
  The lock is released when the guard goes out of scope.
* `Arc<Mutex<T>>` is the shared mutable state pattern used here for audio buffers
  and the current recording language.
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
3. `commands::start_recording` decides whether local Whisper is needed, creates
   shared `CaptureBuffers`, and starts an audio owner thread.
4. `audio::build_capture_streams` resolves the selected CPAL devices. For system
   audio it uses Windows WASAPI output devices as capture sources.
5. Each CPAL callback converts samples to `f32`, downmixes to mono, resamples to
   16 kHz, and appends data to both the full recording buffer and pending
   transcription buffer.
6. A Tokio task drains pending buffers. Local mode transcribes chunks with
   Whisper. Azure mode emits `audio-chunk` events for the React Azure Speech
   session.
7. `stop_recording` stops the async task and audio thread, mixes the full buffers,
   writes a WAV file, and updates meeting metadata.

## Azure CLI Auth In Multi-Tenant Environments

Azure Speech uses bearer tokens from the local Azure CLI account. Multi-tenant
auth is handled in two layers:

* `sign_in_azure_cli` runs `az logout --only-show-errors` first. This clears
  cached CLI accounts so the next `az login` shows the browser account picker.
* `sign_in_azure_cli` then runs `az login --allow-no-subscriptions`. If a tenant
  is configured, it adds `--tenant <tenant-id>`.
* If a subscription is configured, it runs `az account set --subscription
  <subscription-id>` after login.
* `check_azure_cli_sign_in` uses `AzureCliCredential` with the configured tenant
  and subscription to confirm that a management token can be issued without a
  prompt.
* `get_azure_cli_access_token` uses `AzureCliCredential` to request the Azure
  Speech scope: `https://cognitiveservices.azure.com/.default`.

The explicit logout is important. Without it, `az login` may silently reuse the
previous identity. That can fail with `AADSTS50020` when the reused account is
not a member or guest of the selected tenant.

## Where To Make Common Changes

* Add or rename a frontend command wrapper in `src/nativeClient.ts`, then add or
  rename the matching `#[tauri::command]` function in `src-tauri/src/commands/`.
  Tauri maps camelCase TypeScript arguments to snake_case Rust parameters.
* Add fields to `Meeting` in `src-tauri/src/lib.rs` and `src/types.ts`. Use
  `#[serde(default)]` when old stored JSON files should continue loading.
* Change audio capture behavior in `src-tauri/src/audio/mod.rs`.
* Change recording orchestration, transcript persistence, exports, playback
  commands, or Tauri events in `src-tauri/src/commands/mod.rs`.
* Change Azure tenant, subscription, token, or CLI error handling in
  `src-tauri/src/azure_auth.rs`.
* Change Whisper model discovery in `fallback_model_dirs` or `find_default_model`
  in `src-tauri/src/lib.rs`.

## Maintenance Tips

* Keep CPAL audio callbacks small. Heavy work belongs in the Tokio task, not on
  the audio callback thread.
* Prefer `Result<..., String>` for Tauri commands so frontend errors stay
  readable.
* Do not edit `ref-meetily/`; it is reference-only.
* Keep local-first behavior intact. Do not add network calls for local Whisper
  audio, transcripts, or model processing unless the product scope changes.
* Run `npm run build` after TypeScript or React changes.
* Run the README `cargo check` command from a VS 2022 Build Tools environment
  after Rust changes.

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
* `match value { ... }` checks every meaningful case of an enum or pattern. The
  compiler can warn when a case is missing.
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
* `mod audio;` declares a module from `audio.rs` or `audio/mod.rs`.
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

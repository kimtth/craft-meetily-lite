---
title: Meetly Lite Architecture
description: Architecture, native API, and build guidance for Meetly Lite
---

Meetly Lite is generated at the workspace root. The reference project in `ref-meetily/` is read-only input and must not be modified.

## Features

* Compact side-panel transcription UI inspired by Microsoft Teams meeting panels
* Rust/Tauri recording control flow
* Local Whisper model loading through `whisper-rs`
* Optional Azure Speech live recognition and Fast Transcription for selected WAV, MP3, and MP4 files
* Selectable microphone input capture through `cpal`
* Simultaneous microphone and system audio capture with mixed recording output
* Capture modes for microphone only, system audio only, or microphone plus system audio
* Per-session transcription language selection
* Transcript segments emitted from Rust to the UI through Tauri events
* Local meeting and transcript history stored under the app data directory
* Transcript export as `.txt` and recorded audio export in the stored file format
* Recorded audio playback with per-transcript-segment navigation
* Desktop and user-selected-area screen recording with FFmpeg H.264 or H.265 encoding
* Local video catalog, export folder selection, and incomplete-recording recovery
* Optional build-time Whisper acceleration for CUDA or Vulkan
* No AI summary workflow

## Current Implementation Slice

The current root implementation is a Vite and React frontend wrapped by a
Windows-first Tauri application. The backend is the Rust core under
`src-tauri/`, matching the supported architecture described in
`ref-meetily/docs/architecture.md`.

## Backend Boundary

The active backend lives under `src-tauri/` at the workspace root. The older
Node.js file-store backend was removed because it did not perform native audio
capture or transcription.

```mermaid
flowchart TD
	UI[React side-panel UI]
	Tauri[Tauri command bridge]
	Rust[Rust backend core]
	Audio[cpal microphone capture]
	Whisper[whisper-rs transcription]
	Azure[Azure Speech optional cloud transcription]
	Screen[FFmpeg screen recording]
	Store[Local app data store]
	Exports[Transcript, audio, and video exports]

	UI --> Tauri
	Tauri --> Rust
	Rust --> Audio
	Rust --> Whisper
	Rust --> Azure
	Rust --> Screen
	Rust --> Store
	Rust --> Exports
	Rust -- transcript-segment events --> UI
```

## Development

Install dependencies:

```powershell
npm install
```

Run the web frontend only:

```powershell
npm run dev
```

Run the Tauri desktop app:

```powershell
npm run tauri:dev
```

Frontend build validation:

```powershell
npm run build
```

Rust backend validation from a VS 2022 Build Tools developer environment:

```powershell
cmd.exe /c 'call "%ProgramFiles(x86)%\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat" && set "LIBCLANG_PATH=%ProgramFiles%\LLVM\bin" && set "PATH=%ProgramFiles%\CMake\bin;%ProgramFiles%\LLVM\bin;%PATH%" && cd /d "%CD%\src-tauri" && cargo check'
```

The Rust core stores app data in the OS app-data directory selected by Tauri.

## Production

Build the Windows desktop app:

```powershell
npm install
npm run tauri:build
```

Build with automatic Whisper acceleration detection:

```powershell
npm run tauri:build:gpu
```

Build with a specific Whisper backend:

```powershell
npm run tauri:build:cuda
npm run tauri:build:vulkan
```

GPU acceleration is selected at build time. It is not a runtime setting in the app.
Use `cuda` for NVIDIA GPUs with CUDA Toolkit installed, and `vulkan` for supported
GPU drivers with Vulkan tooling. You can also set `TAURI_GPU_FEATURE` to
`cuda` or `vulkan` before running `npm run tauri:build:gpu`.
GPU builds use a short Cargo target directory at the project drive root (`<drive>:\mtg`), and
prefer the Ninja CMake generator bundled with Visual Studio Build Tools to avoid
Windows path-length and MSBuild FileTracker failures in generated Whisper/Vulkan
CMake files.

The default `npm run tauri:build` creates these files:

* `src-tauri/target/release/meetly-lite.exe`
* `src-tauri/target/release/bundle/nsis/Meetly Lite_0.1.0_x64-setup.exe`
* `src-tauri/target/release/bundle/msi/Meetly Lite_0.1.0_x64_en-US.msi`

GPU builds (`tauri:build:gpu`, `tauri:build:cuda`, and `tauri:build:vulkan`) redirect the Cargo target directory to `<drive>:\mtg`, so their
output lands here instead:

* `<drive>:\mtg\release\meetly-lite.exe`
* `<drive>:\mtg\release\bundle\nsis\Meetly Lite_0.1.0_x64-setup.exe`
* `<drive>:\mtg\release\bundle\msi\Meetly Lite_0.1.0_x64_en-US.msi`

Install or run a GPU build from `<drive>:\mtg`. GPU builds never update the
`src-tauri/target/release` files, so those paths keep stale code from an earlier
default build. Running the old `src-tauri/target/release/meetly-lite.exe` after a
GPU build is the most common cause of "my changes did not take effect."

Use the installer for normal testing. Running the raw `meetly-lite.exe` also
works, but it does not install shortcuts or app metadata.

## Whisper Model Files

The executable does not bundle a Whisper model. Download or copy a local
`ggml-*.bin` model file and load it from the app settings with an absolute path.

Download models from the official whisper.cpp model hosting location:

* <https://huggingface.co/ggerganov/whisper.cpp/tree/main>

Recommended starter models:

* `ggml-base.en.bin` for English-only testing with a small download size
* `ggml-base.bin` for multilingual testing
* `ggml-small.en.bin` for better English accuracy when you can use more CPU and disk

Create a local model folder and place the downloaded file there:

```powershell
New-Item -ItemType Directory -Force "$HOME\models"
```

Example model path:

```text
%USERPROFILE%\models\ggml-base.en.bin
```

The file must meet these conditions:

* It exists on the local machine.
* It is a file, not a folder.
* It has a `.bin` extension.
* It is a Whisper `ggml` model compatible with `whisper.cpp`.

If model loading fails in the executable, check the exact path first. The app
does not resolve paths relative to the executable directory. Use the full path
shown in File Explorer.

## Recommended Verification Flow

1. Install or run the built desktop app.
2. Open `Settings`.
3. Select `Browse` next to `Whisper Model Path`.
4. Choose the downloaded `ggml-*.bin` model file.
5. Select `Load Model`.
6. Choose the capture mode, microphone input, system audio output, and session language.
7. Start recording from the microphone button.

Do not use `npm run dev` to test model loading. The browser-only Vite preview
cannot call Tauri native commands and cannot load Whisper models.

## Native Backend Commands

The root Tauri backend provides these commands to the frontend:

* `get_meetings`
* `refresh_meetings`
* `get_videos`
* `refresh_videos`
* `delete_video`
* `rename_video`
* `get_recording_runtime_status`
* `list_audio_input_devices`
* `list_audio_output_devices`
* `list_screen_targets`
* `open_area_selector`
* `complete_area_selection`
* `cancel_area_selection`
* `select_video_output_folder`
* `select_ffmpeg_executable`
* `open_video_recordings_folder`
* `load_whisper_model`
* `select_whisper_model`
* `get_whisper_model_status`
* `get_settings`
* `save_settings`
* `start_recording`
* `stop_recording`
* `start_screen_recording`
* `stop_screen_recording`
* `select_fast_transcription_audio`
* `transcribe_fast_audio`
* `add_transcript_segment`
* `get_azure_cli_access_token`
* `sign_in_azure_cli`
* `check_azure_cli_sign_in`
* `rename_meeting`
* `update_meeting_language`
* `delete_meeting`
* `export_transcript`
* `export_audio`
* `open_meeting_folder`
* `play_recording`
* `pause_playback`
* `resume_playback`
* `set_mini_mode`

## Validation Status

The following checks pass:

* `npm run build`
* `cargo check` from the VS 2022 Build Tools developer environment with `LIBCLANG_PATH` set to `C:\Program Files\LLVM\bin`

The remaining manual validation is a real recording and transcription run after
loading a local Whisper model file.

## Azure Speech Fast Transcription

Fast Transcription accepts one WAV, MP3, or MP4 file. Before upload, the native
backend validates the file type, a 250 MB maximum file size, and a two-hour
maximum duration. It accepts only an HTTPS Azure Cognitive Services custom
domain endpoint and disables redirects before sending the bearer token.

MP4 input is converted locally to a temporary MP3 through the configured FFmpeg
runtime. The temporary file is removed after the request completes. Meetly sends
audio directly to Azure Speech and does not use Azure Blob Storage, storage
account keys, or SAS URLs. Azure CLI obtains the Microsoft Entra access token;
the signed-in identity needs the `Cognitive Services User` role on the Speech
resource.

After Azure Speech returns timestamped phrases, Meetly stores the transcript and
a local copy of the submitted audio as a meeting. The UI reports validation,
upload, transcription, and saving stages while the synchronous request runs.

## Screen Recording Finalization and Recovery

Screen recording uses FFmpeg to create a fragmented `.partial.mp4` in the
configured output directory. When the selected capture mode includes audio,
native audio capture stages a WAV file in the system temporary directory.

Recorded videos can be renamed from the video list. `rename_video` updates the
catalog title and `updatedAt` timestamp in the local store without renaming the
completed MP4 file on disk.

H.264 and H.265 output uses `yuv420p`, so the backend normalizes selected-area
dimensions to even values. On stop, the backend stops FFmpeg and audio capture,
muxes staged audio when available, validates the completed output, then renames
the result to its final `.mp4` file. It emits `screen-processing-status` events
for the processing stages.

At startup and before `refresh_videos` reads the output directory, Meetly retries
inactive incomplete recordings with a nonempty partial MP4. Empty partial files
are marked as capture failures. Intermediate `.partial.mp4` and `.muxing.mp4`
files are not imported into the saved-video catalog.

## Audio Encoding Recovery

Meeting recordings are first finalized as WAV files and then encoded to MP3. If
encoding fails or the app closes before the conversion completes, the WAV stays
associated with the meeting. At the next app startup, Meetly retries conversion
for persisted non-file-transcription WAV recordings. It updates the meeting to
the MP3 path and deletes the WAV only after the updated store is written.

## Native Runtime Flow

```mermaid
flowchart TD
	UI[React side-panel UI]
	Engine[Local Whisper or Azure Speech]
	Record[Start audio or screen recording]
	Audio[Microphone and system audio stream]
	STT[Transcription chunks]
	Screen[FFmpeg screen capture]
	Events[Tauri transcript events]
	Stop[Stop and finalize recording]
	Files[MP3, MP4, and transcript exports]

	UI --> Engine
	UI --> Record
	Record --> Audio
	Audio --> STT
	Record --> Screen
	STT --> Events
	Events --> UI
	UI --> Stop
	Stop --> Files
```

## Removed From Scope

The lightweight version excludes these reference-project capabilities:

* AI-powered meeting summaries
* Summary templates and editor workflow
* Ollama, Claude, Groq, OpenRouter, OpenAI-compatible, and built-in summary providers
* macOS and Linux support paths as product targets

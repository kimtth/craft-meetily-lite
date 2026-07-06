---
title: Meetly Light
description: Lightweight Windows-first local meeting transcription app
---

# Meetly Light

Meetly Light is a Windows-first, local-first meeting transcription app generated at the repository root. The reference implementation in `ref-meetily/` is used only for architecture and product guidance.

## Current Features

* Compact side-panel transcription UI
* Rust/Tauri recording control flow
* Local Whisper model loading through the Rust core
* Microphone capture through `cpal`
* Transcript segments emitted from Rust to the UI through Tauri events
* Local meeting and transcript history stored under the app data directory
* Transcript export as `.txt`
* Recorded audio export as `.wav`
* Recorded audio playback with per-transcript-segment navigation
* No AI summary workflow

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
cmd.exe /c 'call "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat" && set "LIBCLANG_PATH=C:\Program Files\LLVM\bin" && set "PATH=C:\Program Files\CMake\bin;C:\Program Files\LLVM\bin;%PATH%" && cd /d "%CD%\src-tauri" && cargo check'
```

The Rust core stores app data in the OS app-data directory selected by Tauri.

## Production

Build the Windows desktop app:

```powershell
npm install
npm run tauri:build
```

The build creates these files:

* `src-tauri/target/release/meetly-lite.exe`
* `src-tauri/target/release/bundle/nsis/Meetly Light_0.1.0_x64-setup.exe`
* `src-tauri/target/release/bundle/msi/Meetly Light_0.1.0_x64_en-US.msi`

Use the installer for normal testing. Running the raw `meetly-lite.exe` also
works, but it does not install shortcuts or app metadata.

### Whisper model files

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
New-Item -ItemType Directory -Force C:\models
```

Example model path:

```text
C:\models\ggml-base.en.bin
```

The file must meet these conditions:

* It exists on the local machine.
* It is a file, not a folder.
* It has a `.bin` extension.
* It is a Whisper `ggml` model compatible with `whisper.cpp`.

If model loading fails in the executable, check the exact path first. The app
does not resolve paths relative to the executable directory. Use the full path
shown in File Explorer.

### Recommended verification flow

1. Install or run the built desktop app.
2. Open `Settings`.
3. Select `Browse` next to `Whisper Model Path`.
4. Choose the downloaded `ggml-*.bin` model file.
5. Select `Load Model`.
6. Start recording from the microphone button.

Do not use `npm run dev` to test model loading. The browser-only Vite preview
cannot call Tauri native commands and cannot load Whisper models.


## Native Backend Commands

The root Tauri backend provides these commands to the frontend:

* `get_meetings`
* `load_whisper_model`
* `select_whisper_model`
* `get_whisper_model_status`
* `start_recording`
* `stop_recording`
* `rename_meeting`
* `delete_meeting`
* `export_transcript`
* `export_audio`
* `open_meeting_folder`
* `play_recording`
* `pause_playback`
* `resume_playback`
* `set_mini_mode`

## Reference Project Boundary

Do not edit `ref-meetily/`. Generate and modify implementation files at the workspace root.

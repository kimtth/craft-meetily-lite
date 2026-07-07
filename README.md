# Meetly Lite

Meetly Lite is a Windows-first, local-first meeting transcription app. Audio
capture, recording, and Whisper transcription run entirely on your machine.

## Quick Start

```powershell
npm install
npm run tauri:dev
```

Build the Windows desktop app:

```powershell
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

Transcription needs a local Whisper `ggml-*.bin` model. Download one from
<https://huggingface.co/ggerganov/whisper.cpp/tree/main>, then load it from
Settings or place it in a `models/` folder for auto-detection.

> [!NOTE]
> Do not use `npm run dev` to test transcription. The browser-only preview
> cannot call the native Tauri commands.

## Documentation

[Architecture.md](Architecture.md) covers features, build variants (CUDA and
Vulkan) and their output locations, Whisper model setup, the verification flow,
native backend commands, and design details.

## Reference Project Boundary

Do not edit `ref-meetily/`. It is read-only reference input. Generate and modify
implementation files at the workspace root.

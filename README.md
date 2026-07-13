## Meetly Lite

Windows-first meeting transcription with Local Whisper for on-device recording and optional Azure Speech for live or file transcription.

## Start

```powershell
npm install
npm run tauri:dev
```

Build with `npm run tauri:build`. Select a local `ggml-*.bin` Whisper model in Settings, or place one in `models/` for auto-detection. Do not use `npm run dev` for native features.

Builds run `npm run prepare:ffmpeg` first, which downloads the pinned FFmpeg 8.1.2 essentials build into the bundle resources only when it is missing or fails its checksum. GPU-accelerated builds (`tauri:build:cuda`, `tauri:build:vulkan`) and their output paths are documented in [Architecture.md](Architecture.md).

## Azure Speech

Azure Speech uses Azure CLI sign-in and requires the `Cognitive Services User` role. File transcription accepts one WAV, MP3, or MP4 file up to 250 MB and two hours, uploads it directly to Azure Speech, and stores the audio and timestamped transcript locally. For MP4 input, Meetly extracts MP3 locally before upload. Azure Speech does not manage that conversion. Azure Blob Storage is not used.

> [!IMPORTANT]
> Azure Speech sends audio to a cloud service. Use Local Whisper when audio must remain on the device.

See [Architecture.md](Architecture.md) for build variants and implementation details.

## Screen recording

Screen recording captures an entire desktop or a fixed area to local MP4 with H.264 or H.265 encoding. Choose an output folder from the Video tab in Settings. FFmpeg performs capture, compression, audio muxing, and automatic MP4-to-MP3 conversion during Azure Speech Fast Transcription. 

The bundled FFmpeg runtime is used by default. To use a different executable, select it from the Video settings. Video recording is separate from transcription; use the transcript icon on a saved video to send its audio directly to Azure Speech Fast Transcription. 

Meetly writes a fragmented `.partial.mp4` while recording, mixes staged audio in after stop, validates the result, then commits the final `.mp4`. Incomplete recordings are retried when the app reopens or the video list is refreshed. See [Architecture.md](Architecture.md) for finalization, recovery, and encoding details.

### FFmpeg licensing

The bundled FFmpeg 8.1.2 essentials build is a static GPLv3 build from Gyan Doshi's Windows FFmpeg builds. <https://github.com/FFmpeg/FFmpeg/commit/38b88335f9>.

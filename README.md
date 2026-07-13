## Meetly Lite

Windows-first **meeting transcription** and **screen recording** with Local Whisper for on-device recording and optional Azure Speech for live or file transcription.

<table>
	<tr>
		<td align="center">
			<img src="docs/screen_a.png" alt="Meetly Lite meeting transcription workspace" />
		</td>
		<td align="center">
			<img src="docs/screen_b.png" alt="Meetly Lite Azure Speech batch transcription workspace" />
		</td>
		<td align="center">
			<img src="docs/screen_v.png" alt="Meetly Lite screen recording workspace" />
		</td>
	</tr>
	<tr>
		<td align="center"><sub>Meeting transcription with Local Whisper or Azure Speech</sub></td>
		<td align="center"><sub>Azure Speech batch transcription for WAV, MP3, and MP4 files</sub></td>
		<td align="center"><sub>Screen recording for an entire display or selected area</sub></td>
	</tr>
</table>

## Start

```powershell
npm install
npm run tauri:dev
```

* Build with `npm run tauri:build`.
* Select a local `ggml-*.bin` Whisper model in Settings, or place one in `models/` for auto-detection.
* Do not use `npm run dev` for native features.
* Builds run `npm run prepare:ffmpeg` first, which downloads FFmpeg 8.1.2 when needed.
* GPU build commands and output paths are listed in [Architecture.md](Architecture.md).

## Speech-to-Text Engine

* Sign in to Azure CLI and assign the `Cognitive Services User` role.
* Transcribe one WAV, MP3, or MP4 file up to 250 MB and two hours.
* Meetly uploads files directly to Azure Speech and stores audio and timestamped transcripts locally.
* MP4 audio is extracted locally as MP3 before upload.
* Azure Blob Storage is not used.

> [!IMPORTANT]
> Azure Speech sends audio to the cloud. Use Local Whisper to keep audio on the device.

## Screen recording

* Record an entire desktop or fixed area to local H.264 or H.265 MP4.
* Choose the output folder in Video settings.
* FFmpeg handles capture, encoding, audio mixing, and MP4-to-MP3 conversion.
* The bundled FFmpeg runtime is used by default. You can select another executable.
* Transcription is separate. Use a saved video's transcript icon to send its audio to Azure Speech.
* Recordings use `.partial.mp4`, then mix audio, validate, and commit the final `.mp4`.
* Incomplete recordings retry on app reopen or video list refresh.

### FFmpeg licensing

The bundled FFmpeg 8.1.2 essentials build is static GPLv3 software from Gyan Doshi's Windows builds: <https://github.com/FFmpeg>.

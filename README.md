## Meetly Lite

Windows-first **meeting transcription** and **screen recording** with Foundry Local for on-device recording and optional Azure Speech for live or file transcription.

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
		<td align="center"><sub>Meeting transcription with Foundry Local or Azure Speech</sub></td>
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
* Do not use `npm run dev` for native features.
* Builds run `npm run prepare:ffmpeg` first, which downloads FFmpeg 8.1.2 when needed.
* Builds stage the Foundry Local runtime for Windows installers.

## Speech-to-Text Engine

For Foundry Local, select and prepare a compatible speech model in Settings. Utterance-based chunking uses WebRTC VAD; fixed five-second chunking is also available.

* Sign in to Azure CLI and assign the `Cognitive Services User` role.
* Transcribe one WAV, MP3, or MP4 file up to 250 MB and two hours.
* Meetly uploads files directly to Azure Speech and stores audio and timestamped transcripts locally.
* MP4 audio is extracted locally as MP3 before upload.
* Azure Blob Storage is not used.

> [!IMPORTANT]
> Azure Speech sends audio to the cloud. Use Foundry Local to keep audio on the device.

On Windows 11, microphone recordings follow the Teams mute state when an active Teams call is available. Muted microphone frames are saved as silence to preserve synchronization.

## Meeting assistant (GitHub Copilot)

Open a saved or currently recording meeting and click **Copilot · GHCP** in the
top toolbar. A right-side conversation pane opens beside the transcript, inspired
by [Teams meeting Copilot](https://support.microsoft.com/en-us/office/use-copilot-in-microsoft-teams-meetings-0bf9dd3c-96f7-44e2-8bb8-790bedf066b1).
This is GitHub Copilot integration, not a Teams/Microsoft 365 connection.
Open **Set up Copilot for this meeting**, click **Check Copilot status**, select a
model. Send/Generate authorizes transcript, question and assistant-history sharing with GitHub Copilot; there is no separate consent checkbox. Suggested prompts fill the
composer for review; they do not send automatically. Enter sends; Shift+Enter adds
a newline (IME composition does not submit). Ask follow-ups without generating a recap first.

Chat remains available **during recording** once confirmed transcript text exists.
Each submission snapshots that text on the server. New speech does not cancel a
pending answer and is included on the next request. Answers show historical coverage:
the maximum included segment start time and segment count, not audio duration.
Edits/deletions of snapshot evidence reject an in-flight result; unsafe old history
is excluded from subsequent requests, without deleting saved conversations.
Recap and action items are manually generated snapshots in their own tabs. Sources
jump to audio when not recording; playback is disabled during capture. Owners and
deadlines remain unspecified without transcript evidence. On narrow windows, the
recordings sidebar is hidden while Copilot is open to preserve room for the conversation.

The official Copilot Rust SDK/runtime is bundled with the app. Authenticate with
GitHub CLI (`gh auth login`) using a Copilot-enabled account, or configure a supported
`COPILOT_GITHUB_TOKEN`, `GH_TOKEN`, or `GITHUB_TOKEN` in the desktop process environment.
Never paste tokens into a meeting or chat. Organization policy and Copilot usage
allowances still apply. Status checks do not send meeting content; generation can
require multiple requests for a long meeting. Shell, filesystem, MCP, and agent tools
are disabled. History is local and meeting-specific; deleting a meeting removes its
assistant data. Responses appear after validation, not as token streams.
Each submission permits at most one extra paid generation if validation fails.
Models cite short request-local IDs; only validated references are mapped back to
original transcript segments. Exact repeated references are deduplicated, while
missing/unknown evidence still rejects the response without replacing saved data.

## Language, timestamps, and speakers

New recordings use engine defaults. Continued recordings use the selected meeting's
language. Language changes during capture apply to subsequent recognition; a saved
meeting's language selector edits metadata, not previously recognized text.

Use **Retranscribe** on saved audio to open the Azure dialog. Select the language,
optionally enable speaker separation, and explicitly consent to audio upload and
Azure charges. The result is a **new meeting**: original audio/text are preserved.
Anonymous speaker IDs are not verified identities; use **Speaker names** to assign
names locally. Names are included in transcript export.

New capture uses one sample-based recording/transcription timeline; Azure restart
offsets are anchored to delivered audio rather than the UI timer. Existing broken
timestamps are not silently sorted or shifted. Re-transcription requires approval
and is the available recovery workflow when original session anchors are missing.

## Offline validation

`npm run test:audio` runs mocked speech-session and timestamp regression tests.
`npm run test:assistant` checks snapshot change detection, coverage, and composer keys offline.
`npm run build` validates the frontend. `cargo test --manifest-path src-tauri/Cargo.toml --lib`
runs Rust tests in a Visual Studio 2022 x64 Developer Command Prompt on this machine.
The ignored `local_bundled_runtime_handshake` test can be explicitly selected with
`-- --ignored --test-threads=1`; it checks local Copilot transport without inference.
Real recording devices, Azure quality, Copilot entitlement, and cloud answers need
separate user-approved end-to-end validation.

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

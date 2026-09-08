import { invoke, isTauri } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import type {
  AudioInputDevice,
  AudioOutputDevice,
  AppSettings,
  FastTranscriptionFile,
  FastTranscriptionProgress,
  FoundryDownloadProgress,
  FoundryLocalModelCatalogEntry,
  Meeting,
  RecordingOptions,
  RecordingRuntimeStatus,
  ScreenAudioLevels,
  ScreenProcessingStatus,
  ScreenTarget,
  TranscriptSegment,
  VideoRecording,
} from './types';

const FALLBACK_MEETINGS_KEY = 'meetly-lite:fallback-meetings';
const FALLBACK_SETTINGS_KEY = 'meetly-lite:settings';

function hasTauriRuntime(): boolean {
  return typeof window !== 'undefined' && isTauri();
}

function readFallbackJson<T>(key: string, fallback: T): T {
  try {
    const raw = localStorage.getItem(key);
    return raw ? (JSON.parse(raw) as T) : fallback;
  } catch {
    return fallback;
  }
}

function requireTauriRuntime(action: string): never {
  throw new Error(`${action} requires the Tauri desktop app. Run npm run tauri:dev instead of the browser-only dev server.`);
}

export type NativeTranscriptEvent = {
  meetingId: string;
  segment: TranscriptSegment;
};

export type NativeAudioChunkEvent = {
  meetingId: string;
  offsetSeconds: number;
  pcmBase64: string;
};

export type MicrophoneMuteChangedEvent = {
  meetingId: string;
  muted: boolean;
};

export type CallMuteWarningEvent = {
  meetingId: string;
  message: string;
};

export type NativeRecordingErrorEvent = {
  meetingId: string;
  message: string;
  fatal: true;
  recordingPath: string;
};

export type AzureCliAccessToken = {
  token: string;
  expiresOnTimestamp: number;
};

export type AzureCliStatus = {
  id: string;
  state: 'connected' | 'unconfigured' | 'error';
  detail: string;
  account?: string | null;
};

export async function fetchMeetings(): Promise<Meeting[]> {
  if (!hasTauriRuntime()) {
    return readFallbackJson<Meeting[]>(FALLBACK_MEETINGS_KEY, []);
  }
  return invoke<Meeting[]>('get_meetings');
}

export async function refreshMeetings(): Promise<Meeting[]> {
  if (!hasTauriRuntime()) {
    return readFallbackJson<Meeting[]>(FALLBACK_MEETINGS_KEY, []);
  }
  return invoke<Meeting[]>('refresh_meetings');
}

export async function getNativeSettings(): Promise<AppSettings | null> {
  if (!hasTauriRuntime()) {
    return readFallbackJson<AppSettings | null>(FALLBACK_SETTINGS_KEY, null);
  }
  return invoke<AppSettings>('get_settings');
}

export async function saveNativeSettings(settings: AppSettings): Promise<void> {
  if (!hasTauriRuntime()) {
    try {
      localStorage.setItem(FALLBACK_SETTINGS_KEY, JSON.stringify(settings));
    } catch {
      // Ignore storage quota or serialization errors in the browser preview.
    }
    return;
  }
  return invoke<void>('save_settings', { settings });
}

export async function listNativeAudioInputDevices(): Promise<AudioInputDevice[]> {
  if (!hasTauriRuntime()) {
    return [{ id: '', name: 'System default', isDefault: true }];
  }
  return invoke<AudioInputDevice[]>('list_audio_input_devices');
}

export async function listNativeAudioOutputDevices(): Promise<AudioOutputDevice[]> {
  if (!hasTauriRuntime()) {
    return [{ id: '', name: 'System default', isDefault: true }];
  }
  return invoke<AudioOutputDevice[]>('list_audio_output_devices');
}

export async function startNativeRecording(options: RecordingOptions = {}): Promise<Meeting> {
  if (!hasTauriRuntime()) {
    return requireTauriRuntime('Recording');
  }
  return invoke<Meeting>('start_recording', {
    meetingTitle: options.meetingTitle,
    appendToMeetingId: options.appendToMeetingId,
    audioDeviceId: options.audioDeviceId,
    systemAudioDeviceId: options.systemAudioDeviceId,
    captureMode: options.captureMode,
    language: options.language,
    transcriptionEngine: options.transcriptionEngine,
    foundryLocalModelAlias: options.foundryLocalModelAlias,
    foundryLocalChunkingMode: options.foundryLocalChunkingMode,
  });
}

export async function addNativeTranscriptSegment(meetingId: string, text: string, offsetSeconds: number): Promise<TranscriptSegment> {
  if (!hasTauriRuntime()) {
    return requireTauriRuntime('Transcript persistence');
  }
  return invoke<TranscriptSegment>('add_transcript_segment', { meetingId, text, offsetSeconds });
}

export async function getAzureCliAccessToken(tenantId?: string, subscriptionId?: string): Promise<AzureCliAccessToken> {
  if (!hasTauriRuntime()) {
    return requireTauriRuntime('Azure CLI authentication');
  }
  return invoke<AzureCliAccessToken>('get_azure_cli_access_token', { tenantId, subscriptionId });
}

export async function signInAzureCli(tenantId?: string, subscriptionId?: string): Promise<AzureCliStatus> {
  if (!hasTauriRuntime()) {
    return requireTauriRuntime('Azure CLI sign-in');
  }
  return invoke<AzureCliStatus>('sign_in_azure_cli', { tenantId, subscriptionId });
}

export async function checkAzureCliSignIn(tenantId?: string, subscriptionId?: string): Promise<AzureCliStatus> {
  if (!hasTauriRuntime()) {
    return { id: 'azure', state: 'unconfigured', detail: 'Azure CLI authentication requires the Tauri desktop app.' };
  }
  return invoke<AzureCliStatus>('check_azure_cli_sign_in', { tenantId, subscriptionId });
}

export async function stopNativeRecording(): Promise<Meeting> {
  if (!hasTauriRuntime()) {
    return requireTauriRuntime('Recording');
  }
  return invoke<Meeting>('stop_recording');
}

export async function deleteNativeMeeting(meetingId: string): Promise<void> {
  if (!hasTauriRuntime()) {
    const meetings = readFallbackJson<Meeting[]>(FALLBACK_MEETINGS_KEY, []);
    localStorage.setItem(FALLBACK_MEETINGS_KEY, JSON.stringify(meetings.filter((meeting) => meeting.id !== meetingId)));
    return;
  }
  return invoke<void>('delete_meeting', { meetingId });
}

export async function renameNativeMeeting(meetingId: string, title: string): Promise<Meeting> {
  if (!hasTauriRuntime()) {
    const meetings = readFallbackJson<Meeting[]>(FALLBACK_MEETINGS_KEY, []);
    const updated = meetings.map((meeting) => meeting.id === meetingId ? { ...meeting, title } : meeting);
    localStorage.setItem(FALLBACK_MEETINGS_KEY, JSON.stringify(updated));
    const meeting = updated.find((item) => item.id === meetingId);
    if (!meeting) {
      throw new Error('Meeting not found');
    }
    return meeting;
  }
  return invoke<Meeting>('rename_meeting', { meetingId, title });
}

export async function updateNativeMeetingLanguage(meetingId: string, language: string): Promise<Meeting> {
  if (!hasTauriRuntime()) {
    const meetings = readFallbackJson<Meeting[]>(FALLBACK_MEETINGS_KEY, []);
    const updated = meetings.map((meeting) => meeting.id === meetingId ? { ...meeting, language } : meeting);
    localStorage.setItem(FALLBACK_MEETINGS_KEY, JSON.stringify(updated));
    const meeting = updated.find((item) => item.id === meetingId);
    if (!meeting) {
      throw new Error('Meeting not found');
    }
    return meeting;
  }
  return invoke<Meeting>('update_meeting_language', { meetingId, language });
}

export async function renameNativeSpeaker(meetingId: string, speakerId: string, name: string): Promise<Meeting> {
  if (!hasTauriRuntime()) {
    return requireTauriRuntime('Speaker renaming');
  }
  return invoke<Meeting>('rename_speaker', { meetingId, speakerId, name });
}

export async function exportNativeTranscript(meetingId: string): Promise<string | null> {
  if (!hasTauriRuntime()) {
    return requireTauriRuntime('Transcript export');
  }
  return invoke<string | null>('export_transcript', { meetingId });
}

export async function exportNativeAudio(meetingId: string): Promise<string | null> {
  if (!hasTauriRuntime()) {
    return requireTauriRuntime('Audio export');
  }
  return invoke<string | null>('export_audio', { meetingId });
}

export async function openNativeMeetingFolder(meetingId: string): Promise<void> {
  if (!hasTauriRuntime()) {
    return requireTauriRuntime('Open local folder');
  }
  return invoke<void>('open_meeting_folder', { meetingId });
}

export async function playNativeRecording(meetingId: string, offsetSeconds = 0): Promise<void> {
  if (!hasTauriRuntime()) {
    return requireTauriRuntime('Audio playback');
  }
  return invoke<void>('play_recording', { meetingId, offsetSeconds });
}

export async function pauseNativePlayback(): Promise<void> {
  if (!hasTauriRuntime()) {
    return requireTauriRuntime('Audio playback');
  }
  return invoke<void>('pause_playback');
}

export async function resumeNativePlayback(): Promise<void> {
  if (!hasTauriRuntime()) {
    return requireTauriRuntime('Audio playback');
  }
  return invoke<void>('resume_playback');
}

export async function listFoundryLocalModels(): Promise<FoundryLocalModelCatalogEntry[]> {
  if (!hasTauriRuntime()) {
    return [];
  }
  return invoke<FoundryLocalModelCatalogEntry[]>('list_foundry_local_models');
}

export async function downloadFoundryLocalModel(alias: string): Promise<void> {
  if (!hasTauriRuntime()) {
    return requireTauriRuntime('Foundry Local model download');
  }
  return invoke<void>('download_foundry_local_model', { alias });
}

export async function setNativeMiniMode(enabled: boolean): Promise<void> {
  if (!hasTauriRuntime()) {
    return;
  }
  return invoke<void>('set_mini_mode', { enabled });
}

export function onTranscriptSegment(callback: (event: NativeTranscriptEvent) => void): Promise<() => void> {
  if (!hasTauriRuntime()) {
    void callback;
    return Promise.resolve(() => undefined);
  }
  return listen<NativeTranscriptEvent>('transcript-segment', (event) => callback(event.payload));
}

export function onAudioChunk(callback: (event: NativeAudioChunkEvent) => void): Promise<() => void> {
  if (!hasTauriRuntime()) {
    void callback;
    return Promise.resolve(() => undefined);
  }
  return listen<NativeAudioChunkEvent>('audio-chunk', (event) => callback(event.payload));
}

export function onRecordingError(callback: (event: NativeRecordingErrorEvent) => void): Promise<() => void> {
  if (!hasTauriRuntime()) {
    void callback;
    return Promise.resolve(() => undefined);
  }
  return listen<NativeRecordingErrorEvent>('recording-error', (event) => callback(event.payload));
}

export async function selectFastTranscriptionAudio(): Promise<FastTranscriptionFile | null> {
  if (!hasTauriRuntime()) {
    return requireTauriRuntime('File transcription');
  }
  return invoke<FastTranscriptionFile | null>('select_fast_transcription_audio');
}

export function onTranscriptionError(callback: (message: string) => void): Promise<() => void> {
  if (!hasTauriRuntime()) return Promise.resolve(() => undefined);
  return listen<string>('transcription-error', (event) => callback(event.payload));
}

export async function transcribeFastAudio(
  path: string,
  title: string | undefined,
  language: string,
  endpoint: string,
  tenantId?: string,
  subscriptionId?: string,
  uploadConsent = false,
  diarization = false,
): Promise<Meeting> {
  if (uploadConsent !== true) {
    throw new Error('Explicit consent is required before sending this audio to Azure Speech.');
  }
  if (!hasTauriRuntime()) {
    return requireTauriRuntime('File transcription');
  }
  return invoke<Meeting>('transcribe_fast_audio', {
    path,
    title,
    language,
    endpoint,
    tenantId,
    subscriptionId,
    uploadConsent,
    diarization,
  });
}

export function onFastTranscriptionProgress(callback: (progress: FastTranscriptionProgress) => void): Promise<() => void> {
  if (!hasTauriRuntime()) {
    void callback;
    return Promise.resolve(() => undefined);
  }
  return listen<FastTranscriptionProgress>('fast-transcription-progress', (event) => callback(event.payload));
}

export async function fetchVideos(): Promise<VideoRecording[]> {
  if (!hasTauriRuntime()) return [];
  return invoke<VideoRecording[]>('get_videos');
}

export async function refreshVideos(): Promise<VideoRecording[]> {
  if (!hasTauriRuntime()) return [];
  return invoke<VideoRecording[]>('refresh_videos');
}

export async function deleteNativeVideo(videoId: string): Promise<void> {
  if (!hasTauriRuntime()) {
    return requireTauriRuntime('Video deletion');
  }
  return invoke<void>('delete_video', { videoId });
}

export async function renameNativeVideo(videoId: string, title: string): Promise<VideoRecording> {
  if (!hasTauriRuntime()) {
    return requireTauriRuntime('Video renaming');
  }
  return invoke<VideoRecording>('rename_video', { videoId, title });
}

export async function listScreenTargets(): Promise<ScreenTarget[]> {
  if (!hasTauriRuntime()) return [];
  return invoke<ScreenTarget[]>('list_screen_targets');
}

export async function openAreaSelector(): Promise<void> {
  if (!hasTauriRuntime()) return requireTauriRuntime('Area selection');
  return invoke<void>('open_area_selector');
}

export async function completeAreaSelection(selection: {
  x: number;
  y: number;
  width: number;
  height: number;
  viewportWidth: number;
  viewportHeight: number;
}): Promise<void> {
  if (!hasTauriRuntime()) return;
  return invoke<void>('complete_area_selection', selection);
}

export async function cancelAreaSelection(): Promise<void> {
  if (!hasTauriRuntime()) return;
  return invoke<void>('cancel_area_selection');
}

export function onAreaSelected(callback: (target: ScreenTarget) => void): Promise<() => void> {
  if (!hasTauriRuntime()) {
    void callback;
    return Promise.resolve(() => undefined);
  }
  return listen<ScreenTarget>('area-selected', (event) => callback(event.payload));
}

export async function selectVideoOutputFolder(): Promise<string | null> {
  if (!hasTauriRuntime()) return requireTauriRuntime('Video output folder selection');
  return invoke<string | null>('select_video_output_folder');
}

export async function selectFfmpegExecutable(): Promise<string | null> {
  if (!hasTauriRuntime()) return requireTauriRuntime('FFmpeg executable selection');
  return invoke<string | null>('select_ffmpeg_executable');
}

export async function openVideoRecordingsFolder(): Promise<void> {
  if (!hasTauriRuntime()) return requireTauriRuntime('Open recordings folder');
  return invoke<void>('open_video_recordings_folder');
}

export async function startScreenRecording(options: {
  title?: string;
  targetId: string;
  targetName: string;
  x: number;
  y: number;
  width: number;
  height: number;
  codec: string;
  audioDeviceId?: string;
  systemAudioDeviceId?: string;
  captureMode?: string;
}): Promise<VideoRecording> {
  if (!hasTauriRuntime()) return requireTauriRuntime('Screen recording');
  return invoke<VideoRecording>('start_screen_recording', options);
}

export async function stopScreenRecording(): Promise<VideoRecording> {
  if (!hasTauriRuntime()) return requireTauriRuntime('Screen recording');
  return invoke<VideoRecording>('stop_screen_recording');
}

export function onScreenAudioLevel(callback: (levels: ScreenAudioLevels) => void): Promise<() => void> {
  if (!hasTauriRuntime()) {
    void callback;
    return Promise.resolve(() => undefined);
  }
  return listen<ScreenAudioLevels>('screen-audio-level', (event) => callback(event.payload));
}

export function onScreenProcessingStatus(callback: (status: ScreenProcessingStatus) => void): Promise<() => void> {
  if (!hasTauriRuntime()) {
    void callback;
    return Promise.resolve(() => undefined);
  }
  return listen<ScreenProcessingStatus>('screen-processing-status', (event) => callback(event.payload));
}

export function onMicrophoneMuteChanged(
  callback: (event: MicrophoneMuteChangedEvent) => void,
): Promise<() => void> {
  if (!hasTauriRuntime()) {
    void callback;
    return Promise.resolve(() => undefined);
  }
  return listen<MicrophoneMuteChangedEvent>('microphone-mute-changed', (event) => callback(event.payload));
}

export function onCallMuteWarning(
  callback: (event: CallMuteWarningEvent) => void,
): Promise<() => void> {
  if (!hasTauriRuntime()) {
    void callback;
    return Promise.resolve(() => undefined);
  }
  return listen<CallMuteWarningEvent>('call-mute-warning', (event) => callback(event.payload));
}

export function onFoundryDownloadProgress(callback: (event: FoundryDownloadProgress) => void): Promise<() => void> {
  if (!hasTauriRuntime()) {
    void callback;
    return Promise.resolve(() => undefined);
  }
  return listen<FoundryDownloadProgress>('foundry-download-progress', (event) => callback(event.payload));
}

export async function getRecordingRuntimeStatus(): Promise<RecordingRuntimeStatus> {
  if (!hasTauriRuntime()) {
    return {
      audioRecordingActive: false,
      audioElapsedSeconds: 0,
      audioMicrophoneMuted: false,
      screenRecordingActive: false,
      screenElapsedSeconds: 0,
    };
  }
  return invoke<RecordingRuntimeStatus>('get_recording_runtime_status');
}

import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import type { AudioInputDevice, AudioOutputDevice, Meeting, RecordingOptions, TranscriptSegment, WhisperModelStatus } from './types';

const FALLBACK_MEETINGS_KEY = 'meetly-lite:fallback-meetings';

function hasTauriRuntime(): boolean {
  return typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;
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

export async function fetchMeetings(): Promise<Meeting[]> {
  if (!hasTauriRuntime()) {
    return readFallbackJson<Meeting[]>(FALLBACK_MEETINGS_KEY, []);
  }
  return invoke<Meeting[]>('get_meetings');
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
    audioDeviceId: options.audioDeviceId,
    systemAudioDeviceId: options.systemAudioDeviceId,
    captureMode: options.captureMode,
    language: options.language,
  });
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

export async function getWhisperModelStatus(): Promise<WhisperModelStatus> {
  if (!hasTauriRuntime()) {
    return { loaded: false, path: null };
  }
  return invoke<WhisperModelStatus>('get_whisper_model_status');
}

export async function loadWhisperModel(modelPath: string): Promise<void> {
  if (!hasTauriRuntime()) {
    return requireTauriRuntime('Whisper model loading');
  }
  return invoke<void>('load_whisper_model', { modelPath });
}

export async function selectWhisperModel(): Promise<string | null> {
  if (!hasTauriRuntime()) {
    return requireTauriRuntime('Whisper model file selection');
  }
  return invoke<string | null>('select_whisper_model');
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
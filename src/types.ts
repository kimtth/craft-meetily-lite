export type TranscriptSegment = {
  id: string;
  offsetSeconds: number;
  text: string;
};

export type Meeting = {
  id: string;
  title: string;
  createdAt: string;
  updatedAt: string;
  durationSeconds: number;
  transcript: TranscriptSegment[];
  hasAudio: boolean;
  recordingPath?: string | null;
  audioDeviceId?: string | null;
  audioDeviceName?: string | null;
  systemAudioDeviceId?: string | null;
  systemAudioDeviceName?: string | null;
  captureMode: string;
  language: string;
};

export type AudioInputDevice = {
  id: string;
  name: string;
  isDefault: boolean;
};

export type AudioOutputDevice = {
  id: string;
  name: string;
  isDefault: boolean;
};

export type WhisperModelStatus = {
  loaded: boolean;
  path: string | null;
};

export type RecordingOptions = {
  meetingTitle?: string;
  audioDeviceId?: string;
  systemAudioDeviceId?: string;
  captureMode?: string;
  language?: string;
};

export type RecorderStatus = 'idle' | 'recording' | 'saving';
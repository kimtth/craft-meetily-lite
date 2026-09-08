export type TranscriptSegment = {
  id: string;
  offsetSeconds: number;
  text: string;
  speakerId?: string | null;
  speakerName?: string | null;
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
  transcriptionEngine?: MeetingTranscriptionEngine;
};

export type MeetingTranscriptionEngine = 'foundryLocal' | 'azure' | 'azure-fast';

export type FastTranscriptionFile = {
  path: string;
  name: string;
  sizeBytes: number;
  durationSeconds: number;
  requiresMp3Extraction?: boolean;
};

export type VideoRecording = {
  id: string;
  title: string;
  createdAt: string;
  updatedAt: string;
  durationSeconds: number;
  status: 'recording' | 'saved' | string;
  targetName: string;
  codec: 'h264' | 'h265' | string;
  videoPath: string;
  hasAudio?: boolean | null;
  audioPath?: string | null;
  postProcessStage?: string | null;
};

export type ScreenTarget = {
  id: string;
  name: string;
  x: number;
  y: number;
  width: number;
  height: number;
};

export type FastTranscriptionProgress = {
  phase: 'validating' | 'uploading' | 'transcribing' | 'saving';
  detail: string;
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

export type FoundryLocalModelCatalogEntry = {
  alias: string;
  displayName?: string | null;
  task?: string | null;
  modelType: string;
  inputModalities?: string | null;
  outputModalities?: string | null;
  cached: boolean;
  fileSizeMb?: number | null;
};

export type FoundryDownloadProgress = {
  alias: string;
  phase: 'eps' | 'model' | 'done' | 'error';
  percent: number;
  message?: string;
};

export type RecordingOptions = {
  meetingTitle?: string;
  appendToMeetingId?: string;
  audioDeviceId?: string;
  systemAudioDeviceId?: string;
  captureMode?: string;
  language?: string;
  transcriptionEngine?: TranscriptionEngine;
  foundryLocalModelAlias?: string;
  foundryLocalChunkingMode?: FoundryLocalChunkingMode;
};

export type RecorderStatus = 'idle' | 'starting' | 'recording' | 'saving';

export type TranscriptionEngine = 'azure' | 'foundryLocal';
export type FoundryLocalChunkingMode = 'utterance' | 'fixed5Seconds';

export type AppSettings = {
  transcriptionEngine: string;
  captureMode: string;
  screenAudioCaptureMode: string;
  audioDeviceId: string;
  systemAudioDeviceId: string;
  language: string;
  azureEndpoint: string;
  azureTenantId: string;
  azureSubscriptionId: string;
  azureLanguage: string;
  foundryLocalModelAlias: string;
  foundryLocalLanguage: string;
  foundryLocalChunkingMode: FoundryLocalChunkingMode;
  videoOutputFolder: string;
  videoCodec: string;
  ffmpegPath: string;
};

export type ScreenAudioLevels = {
  microphone: number;
  system: number;
  mixed: number;
};

export type ScreenProcessingStatus = {
  stage: string;
  detail: string;
};

export type RecordingRuntimeStatus = {
  audioRecordingActive: boolean;
  audioMeetingId?: string | null;
  audioElapsedSeconds: number;
  audioMicrophoneMuted: boolean;
  screenRecordingActive: boolean;
  screenVideoId?: string | null;
  screenElapsedSeconds: number;
};
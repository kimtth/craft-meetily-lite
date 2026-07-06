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
};

export type RecorderStatus = 'idle' | 'recording' | 'saving';
import { useEffect, useMemo, useRef, useState } from 'react';
import {
  CalendarDays,
  CircleStop,
  Download,
  FileAudio,
  FileText,
  FolderOpen,
  Maximize2,
  MessageSquareText,
  Mic,
  Minimize2,
  Pause,
  Play,
  Search,
  SlidersHorizontal,
  Trash2,
  Monitor,
  Video,
  RefreshCw,
  Volume2,
} from 'lucide-react';
import type { AppSettings, FastTranscriptionFile, FastTranscriptionProgress, FoundryDownloadProgress, FoundryLocalChunkingMode, FoundryLocalModelCatalogEntry, Meeting, RecorderStatus, ScreenAudioLevels, ScreenProcessingStatus, ScreenTarget, TranscriptSegment, TranscriptionEngine, VideoRecording } from './types';
import {
  addNativeTranscriptSegment,
  checkAzureCliSignIn,
  downloadFoundryLocalModel,
  fetchMeetings,
  refreshMeetings,
  deleteNativeMeeting,
  deleteNativeVideo,
  renameNativeVideo,
  exportNativeAudio,
  exportNativeTranscript,
  getNativeSettings,
  getRecordingRuntimeStatus,
  listFoundryLocalModels,
  listNativeAudioInputDevices,
  listNativeAudioOutputDevices,
  onAudioChunk,
  onCallMuteWarning,
  onFoundryDownloadProgress,
  onMicrophoneMuteChanged,
  onRecordingError,
  onTranscriptionError,
  onTranscriptSegment,
  openNativeMeetingFolder,
  onFastTranscriptionProgress,
  pauseNativePlayback,
  playNativeRecording,
  renameNativeMeeting,
  renameNativeSpeaker,
  resumeNativePlayback,
  signInAzureCli,
  saveNativeSettings,
  selectFastTranscriptionAudio,
  startNativeRecording,
  stopNativeRecording,
  setNativeMiniMode,
  updateNativeMeetingLanguage,
  transcribeFastAudio,
  refreshVideos,
  listScreenTargets,
  onAreaSelected,
  openVideoRecordingsFolder,
  openAreaSelector,
  onScreenAudioLevel,
  onScreenProcessingStatus,
  selectFfmpegExecutable,
  selectVideoOutputFolder,
  startScreenRecording,
  stopScreenRecording,
} from './nativeClient';
import { startAzureSpeechSession } from './azureSpeech';
import type { AzureSpeechSession } from './azureSpeech';
import type { AzureCliStatus, NativeAudioChunkEvent, NativeRecordingErrorEvent } from './nativeClient';
import { formatTimestamp } from './exporters';
import { AZURE_LANGUAGE_OPTIONS, CAPTURE_MODE_OPTIONS, LANGUAGE_OPTIONS, normalizeAzureLanguage, normalizeFoundryLanguage } from './audio/recordingOptions';
import { ScreenRecordingWorkspace } from './video/ScreenRecordingWorkspace';
import MeetingAssistant from './assistant/MeetingAssistant';

const FOUNDRY_LANGUAGE_OPTIONS = LANGUAGE_OPTIONS.filter((option) => option.value !== 'auto');

type AzureRecordingPipe = {
  meetingId: string | null;
  session: AzureSpeechSession | null;
  chunks: NativeAudioChunkEvent[];
  bufferedChars: number;
  cleanup: (() => void) | null;
  closed: boolean;
};

// Object identity distinguishes successive capture attempts, including appends
// to the same meeting. Never use the currently selected meeting for cleanup.
type RecordingAttempt = {
  meetingId: string | null;
  pendingErrors: Map<string, NativeRecordingErrorEvent>;
  failure: NativeRecordingErrorEvent | null;
};

// About 90 seconds of base64 PCM. Never grow indefinitely during auth/reconnect.
const MAX_AZURE_BUFFER_CHARS = 4 * 1024 * 1024;
const MAX_AZURE_BUFFER_CHUNKS = 512;
const SPEAKER_BADGE_STYLE = {
  display: 'inline-block', padding: '2px 6px', marginRight: 6,
  borderRadius: 6, background: 'var(--panel-soft)', color: 'var(--muted)', fontSize: 11,
} as const;

function getErrorMessage(error: unknown, fallback: string): string {
  if (error instanceof Error) {
    return error.message;
  }
  if (typeof error === 'string') {
    return error;
  }
  return fallback;
}

function upsertMeeting(meetings: Meeting[], meeting: Meeting): Meeting[] {
  const existing = meetings.findIndex((item) => item.id === meeting.id);
  if (existing === -1) {
    return [meeting, ...meetings];
  }
  const next = [...meetings];
  const current = next[existing];
  next[existing] = {
    ...meeting,
    transcript: meeting.transcript.length >= current.transcript.length ? meeting.transcript : current.transcript,
  };
  return next.sort((a, b) => Date.parse(b.updatedAt) - Date.parse(a.updatedAt));
}

function getRecorderStatusLabel(status: RecorderStatus): string {
  return status === 'idle' ? 'Ready' : `${status[0].toUpperCase()}${status.slice(1)}`;
}

function isTranscriptionEngine(value: string | undefined): value is TranscriptionEngine {
  return value === 'azure' || value === 'foundryLocal';
}

function migrateTranscriptionEngine(value: string | undefined): TranscriptionEngine {
  return isTranscriptionEngine(value) ? value : 'foundryLocal';
}

function foundryModelLabel(model: FoundryLocalModelCatalogEntry): string {
  return model.displayName ? `${model.displayName} (${model.alias})` : model.alias;
}

type RecorderStatusDisplayProps = {
  className: string;
  status: RecorderStatus;
  elapsedSeconds: number;
  microphoneMuted?: boolean;
};

function RecorderStatusDisplay({
  className,
  status,
  elapsedSeconds,
  microphoneMuted = false,
}: RecorderStatusDisplayProps) {
  const isMuted = status === 'recording' && microphoneMuted;
  return (
    <div className={className}>
      <span className={`state-dot ${isMuted ? 'muted' : status}`} />
      <div>
        <strong>{isMuted ? 'Mic muted' : getRecorderStatusLabel(status)}</strong>
        <span>{formatTimestamp(elapsedSeconds)}</span>
      </div>
    </div>
  );
}

type RecordButtonProps = {
  className: string;
  status: RecorderStatus;
  iconSize: number;
  onStart: () => void;
  onStop: () => void;
  title?: string;
};

function RecordButton({ className, status, iconSize, onStart, onStop, title }: RecordButtonProps) {
  const isIdle = status === 'idle';
  const isBusy = status === 'starting' || status === 'saving';
  return (
    <button
      className={className}
      title={title}
      onClick={isIdle ? onStart : onStop}
      disabled={isBusy}
    >
      {status === 'recording' || status === 'saving' ? <CircleStop size={iconSize} /> : <Mic size={iconSize} />}
    </button>
  );
}

export default function App() {
  const [meetings, setMeetings] = useState<Meeting[]>([]);
  const [activeMeetingId, setActiveMeetingId] = useState<string | null>(null);
  const [status, setStatus] = useState<RecorderStatus>('idle');
  const [searchQuery, setSearchQuery] = useState('');
  const [elapsedSeconds, setElapsedSeconds] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const [callMuteWarning, setCallMuteWarning] = useState<string | null>(null);
  const [microphoneMuted, setMicrophoneMuted] = useState(false);
  const [showSettings, setShowSettings] = useState(false);
  const [showRecordingList, setShowRecordingList] = useState(false);
  const [showRecordingChoice, setShowRecordingChoice] = useState(false);
  const [showBatchTranscription, setShowBatchTranscription] = useState(false);
  const [showVideoWorkspace, setShowVideoWorkspace] = useState(false);
  const [settingsTab, setSettingsTab] = useState<'audio' | 'video'>('audio');
  const [pendingMeetingId, setPendingMeetingId] = useState<string | null>(null);
  const [miniMode, setMiniMode] = useState(false);
  const [miniModeTarget, setMiniModeTarget] = useState<'transcription' | 'screen'>('transcription');
  const [playingMeetingId, setPlayingMeetingId] = useState<string | null>(null);
  const [playingSegmentId, setPlayingSegmentId] = useState<string | null>(null);
  const [playbackPaused, setPlaybackPaused] = useState(false);
  const [audioInputDevices, setAudioInputDevices] = useState([{ id: '', name: 'System default', isDefault: true }]);
  const [audioOutputDevices, setAudioOutputDevices] = useState([{ id: '', name: 'System default', isDefault: true }]);
  const [selectedAudioInputId, setSelectedAudioInputId] = useState('');
  const [selectedSystemAudioOutputId, setSelectedSystemAudioOutputId] = useState('');
  const [selectedCaptureMode, setSelectedCaptureMode] = useState('microphoneSystem');
  const [selectedLanguage, setSelectedLanguage] = useState('en');
  const [transcriptionEngine, setTranscriptionEngine] = useState<TranscriptionEngine>('foundryLocal');
  const [azureEndpoint, setAzureEndpoint] = useState('');
  const [azureTenantId, setAzureTenantId] = useState('');
  const [azureSubscriptionId, setAzureSubscriptionId] = useState('');
  const [azureLanguage, setAzureLanguage] = useState('en-US');
  const [azureCliStatus, setAzureCliStatus] = useState<AzureCliStatus | null>(null);
  const [azureSigningIn, setAzureSigningIn] = useState(false);
  const [azurePartialText, setAzurePartialText] = useState('');
  const [fastTranscriptionFile, setFastTranscriptionFile] = useState<FastTranscriptionFile | null>(null);
  const [fastTranscriptionProgress, setFastTranscriptionProgress] = useState<FastTranscriptionProgress | null>(null);
  const [fastTranscribing, setFastTranscribing] = useState(false);
  const [fastLanguage, setFastLanguage] = useState('en-US');
  const [fastUploadConsent, setFastUploadConsent] = useState(false);
  const [fastDiarization, setFastDiarization] = useState(false);
  const [retranscribeSourceId, setRetranscribeSourceId] = useState<string | null>(null);
  const [assistantExpanded, setAssistantExpanded] = useState(false);
  const [recordingOperationBusy, setRecordingOperationBusy] = useState(false);
  const [refreshingMeetings, setRefreshingMeetings] = useState(false);
  const [refreshingVideos, setRefreshingVideos] = useState(false);
  const [videos, setVideos] = useState<VideoRecording[]>([]);
  const [screenTargets, setScreenTargets] = useState<ScreenTarget[]>([]);
  const [screenStatus, setScreenStatus] = useState<RecorderStatus>('idle');
  const [screenElapsedSeconds, setScreenElapsedSeconds] = useState(0);
  const [selectedScreenTarget, setSelectedScreenTarget] = useState<ScreenTarget | null>(null);
  const [videoOutputFolder, setVideoOutputFolder] = useState('');
  const [videoCodec, setVideoCodec] = useState<'h264' | 'h265'>('h264');
  const [ffmpegPath, setFfmpegPath] = useState('');
  const [screenAudioCaptureMode, setScreenAudioCaptureMode] = useState('microphoneSystem');
  const [screenAudioLevels, setScreenAudioLevels] = useState<ScreenAudioLevels>({ microphone: 0, system: 0, mixed: 0 });
  const [screenProcessingStatus, setScreenProcessingStatus] = useState<ScreenProcessingStatus | null>(null);
  const [foundryLocalModels, setFoundryLocalModels] = useState<FoundryLocalModelCatalogEntry[]>([]);
  const [foundryLocalModelAlias, setFoundryLocalModelAlias] = useState('');
  const [foundryLocalChunkingMode, setFoundryLocalChunkingMode] =
    useState<FoundryLocalChunkingMode>('utterance');
  const [foundryLocalLanguage, setFoundryLocalLanguage] = useState('en');
  const [foundryLoadingModels, setFoundryLoadingModels] = useState(false);
  const [foundryDownloadingAlias, setFoundryDownloadingAlias] = useState<string | null>(null);
  const [foundryDownloadProgress, setFoundryDownloadProgress] = useState<FoundryDownloadProgress | null>(null);

  const elapsedTimerRef = useRef<number | null>(null);
  const screenElapsedTimerRef = useRef<number | null>(null);
  const activeScreenVideoIdRef = useRef<string | null>(null);
  const activeMeetingIdRef = useRef<string | null>(null);
  const latestMicrophoneMuteRef = useRef<{ meetingId: string; muted: boolean } | null>(null);
  const elapsedSecondsRef = useRef(0);
  const transcriptListRef = useRef<HTMLDivElement | null>(null);
  const recordingMeetingIdRef = useRef<string | null>(null);
  const azurePipeRef = useRef<AzureRecordingPipe | null>(null);
  const recordingQueueRef = useRef<Promise<void>>(Promise.resolve());
  const recordingOperationsRef = useRef(0);
  const fastTranscribingRef = useRef(false);
  const fastConsentRef = useRef<{ file: FastTranscriptionFile; meetingId: string | null } | null>(null);
  const stopRecordingPromiseRef = useRef<Promise<void> | null>(null);
  const recordingAttemptRef = useRef<RecordingAttempt | null>(null);
  const recordingListenersReadyRef = useRef<Promise<void>>(Promise.resolve());
  const settingsLoadedRef = useRef(false);

  function beginRecordingAttempt(): RecordingAttempt {
    const attempt: RecordingAttempt = { meetingId: null, pendingErrors: new Map(), failure: null };
    recordingAttemptRef.current = attempt;
    return attempt;
  }

  function surfaceRecordingFailure(attempt: RecordingAttempt) {
    if (!attempt.failure || recordingAttemptRef.current !== attempt) return;
    const { message, recordingPath } = attempt.failure;
    const recovery = `${message} Capture stopped. Recovery WAV: ${recordingPath}. No recovery audio has been uploaded automatically.`;
    setError((current) => current?.includes(recovery) ? current : current ? `${current} ${recovery}` : recovery);
  }

  function receiveRecordingFailure(event: NativeRecordingErrorEvent) {
    const attempt = recordingAttemptRef.current;
    if (!attempt || !event.fatal || attempt.failure) return;
    if (!attempt.meetingId) {
      // Native can fail before start_recording returns the new meeting ID.
      attempt.pendingErrors.set(event.meetingId, event);
      return;
    }
    if (event.meetingId !== attempt.meetingId) return;
    // A user Stop may already have cleared the native ID while Azure/local
    // persistence cleanup is still running. Retain a concurrent failure too.
    if (recordingMeetingIdRef.current !== event.meetingId && !stopRecordingPromiseRef.current) return;
    attempt.failure = event;
    stopTimers();
    surfaceRecordingFailure(attempt);
    // Do not await from inside startup: stop is serialized behind that operation.
    // The failure latch and the shared stop promise coalesce repeated events and
    // a simultaneous user Stop without starting a cloud recovery/transcription.
    void handleStopRecording(attempt).catch(() => surfaceRecordingFailure(attempt));
  }

  function identifyRecordingAttempt(attempt: RecordingAttempt, meetingId: string) {
    if (recordingAttemptRef.current !== attempt) return;
    attempt.meetingId = meetingId;
    const pending = attempt.pendingErrors.get(meetingId);
    attempt.pendingErrors.clear();
    if (pending) receiveRecordingFailure(pending);
  }

  function revokeFastConsent() {
    fastConsentRef.current = null;
    setFastUploadConsent(false);
  }

  function enqueueRecordingOperation(action: () => Promise<void>): Promise<void> {
    recordingOperationsRef.current += 1;
    setRecordingOperationBusy(true);
    const operation = recordingQueueRef.current.then(action);
    // A failed operation must not poison subsequent stop/retry requests.
    const settled = operation.catch(() => undefined).finally(() => {
      recordingOperationsRef.current -= 1;
      setRecordingOperationBusy(recordingOperationsRef.current > 0);
    });
    recordingQueueRef.current = settled;
    return operation;
  }

  function flushAzureChunks(pipe: AzureRecordingPipe) {
    if (!pipe.session || !pipe.meetingId || pipe.closed) return;
    const chunks = pipe.chunks;
    pipe.chunks = [];
    pipe.bufferedChars = 0;
    for (const chunk of chunks) {
      if (chunk.meetingId === pipe.meetingId) {
        pipe.session.pushPcmBase64(chunk.pcmBase64, chunk.offsetSeconds);
      }
    }
  }

  async function listenForAzureAudio(meetingId: string | null): Promise<AzureRecordingPipe> {
    const pipe: AzureRecordingPipe = {
      meetingId, session: null, chunks: [], bufferedChars: 0, cleanup: null, closed: false,
    };
    pipe.cleanup = await onAudioChunk((chunk) => {
      if (pipe.closed || (pipe.meetingId && chunk.meetingId !== pipe.meetingId)) return;
      try {
        if (pipe.session && pipe.meetingId) {
          pipe.session.pushPcmBase64(chunk.pcmBase64, chunk.offsetSeconds);
          return;
        }
        pipe.chunks.push(chunk);
        pipe.bufferedChars += chunk.pcmBase64.length;
        let overflow = false;
        while (pipe.bufferedChars > MAX_AZURE_BUFFER_CHARS || pipe.chunks.length > MAX_AZURE_BUFFER_CHUNKS) {
          pipe.bufferedChars -= pipe.chunks.shift()!.pcmBase64.length;
          overflow = true;
        }
        if (overflow) setError('Azure transcription buffer is full. Older pending audio was omitted from live transcription; the local recording is preserved. Stop and retranscribe the saved audio to recover it.');
      } catch (audioError) {
        setError(getErrorMessage(audioError, 'Could not deliver audio to Azure Speech. The local recording is preserved.'));
      }
    });
    azurePipeRef.current = pipe;
    return pipe;
  }

  function createAzureSession(meetingId: string, language: string, connection: {
    endpoint: string; tenantId?: string; subscriptionId?: string;
  }) {
    return startAzureSpeechSession({
      ...connection,
      language: normalizeAzureLanguage(language),
      onPartialText: setAzurePartialText,
      onFinalText: async (text, offsetSeconds) => {
        // The SDK already maps PCM anchors to ABSOLUTE saved-audio offsets.
        await addNativeTranscriptSegment(meetingId, text, offsetSeconds);
        setAzurePartialText('');
      },
      onError: setError,
    });
  }

  async function closeAzurePipe(pipe: AzureRecordingPipe | null) {
    if (!pipe) return;
    pipe.closed = true;
    pipe.cleanup?.();
    pipe.cleanup = null;
    const session = pipe.session;
    pipe.session = null;
    pipe.chunks = [];
    pipe.bufferedChars = 0;
    if (azurePipeRef.current === pipe) azurePipeRef.current = null;
    await session?.stop();
  }

  const settingsSnapshot = (overrides: Partial<AppSettings> = {}): AppSettings => ({
    transcriptionEngine,
    captureMode: selectedCaptureMode,
    screenAudioCaptureMode,
    audioDeviceId: selectedAudioInputId,
    systemAudioDeviceId: selectedSystemAudioOutputId,
    language: selectedLanguage,
    azureEndpoint,
    azureTenantId,
    azureSubscriptionId,
    azureLanguage,
    foundryLocalModelAlias,
    foundryLocalLanguage,
    foundryLocalChunkingMode,
    videoOutputFolder,
    videoCodec,
    ffmpegPath,
    ...overrides,
  });

  const activeMeeting = useMemo(
    () => meetings.find((meeting) => meeting.id === activeMeetingId) ?? null,
    [activeMeetingId, meetings],
  );

  const meetingSpeakers = useMemo(() => {
    const speakers = new Map<string, string>();
    for (const segment of activeMeeting?.transcript ?? []) {
      if (segment.speakerId) speakers.set(segment.speakerId, segment.speakerName || '');
    }
    return [...speakers].map(([id, name]) => ({ id, name }));
  }, [activeMeeting]);

  const filteredMeetings = useMemo(() => {
    const query = searchQuery.trim().toLowerCase();
    if (!query) {
      return meetings;
    }
    return meetings.filter((meeting) => {
      const transcriptText = meeting.transcript.map((segment) => segment.text).join(' ').toLowerCase();
      return meeting.title.toLowerCase().includes(query) || transcriptText.includes(query);
    });
  }, [meetings, searchQuery]);

  const visibleTranscript = useMemo(() => {
    return activeMeeting?.transcript.slice(-7) ?? [];
  }, [activeMeeting]);

  useEffect(() => {
    let disposed = false;
    // Queue restore-time events even while the listener registrations and
    // runtime identity lookup are still pending.
    const restoredAttempt = beginRecordingAttempt();
    const load = async () => {
      try {
        if (disposed) return;
        await recordingListenersReadyRef.current;
        if (disposed) return;
        const attempt = restoredAttempt;
        const storedMeetings = await fetchMeetings();
        setMeetings(storedMeetings);
        setVideos(await refreshVideos());
        const targets = await listScreenTargets();
        setScreenTargets(targets);
        if (targets[0]) {
          setSelectedScreenTarget(targets[0]);
        }
        const storedSettings = await getNativeSettings();
        if (storedSettings) {
          setTranscriptionEngine(migrateTranscriptionEngine(storedSettings.transcriptionEngine));
          if (storedSettings.captureMode) setSelectedCaptureMode(storedSettings.captureMode);
          if (storedSettings.screenAudioCaptureMode) {
            setScreenAudioCaptureMode(storedSettings.screenAudioCaptureMode);
          } else if (storedSettings.captureMode) {
            setScreenAudioCaptureMode(storedSettings.captureMode);
          }
          if (storedSettings.audioDeviceId) setSelectedAudioInputId(storedSettings.audioDeviceId);
          if (storedSettings.systemAudioDeviceId) setSelectedSystemAudioOutputId(storedSettings.systemAudioDeviceId);
          if (storedSettings.language) setSelectedLanguage(normalizeFoundryLanguage(storedSettings.language));
          if (storedSettings.azureEndpoint) setAzureEndpoint(storedSettings.azureEndpoint);
          if (storedSettings.azureTenantId) setAzureTenantId(storedSettings.azureTenantId);
          if (storedSettings.azureSubscriptionId) setAzureSubscriptionId(storedSettings.azureSubscriptionId);
          if (storedSettings.azureLanguage) setAzureLanguage(normalizeAzureLanguage(storedSettings.azureLanguage));
          if (storedSettings.videoOutputFolder) setVideoOutputFolder(storedSettings.videoOutputFolder);
          if (storedSettings.videoCodec === 'h265' || storedSettings.videoCodec === 'h264') setVideoCodec(storedSettings.videoCodec);
          if (storedSettings.ffmpegPath) setFfmpegPath(storedSettings.ffmpegPath);
          if (storedSettings.foundryLocalModelAlias) setFoundryLocalModelAlias(storedSettings.foundryLocalModelAlias);
          setFoundryLocalChunkingMode(
            storedSettings.foundryLocalChunkingMode === 'fixed5Seconds'
              ? 'fixed5Seconds'
              : 'utterance'
          );
          if (storedSettings.foundryLocalLanguage) setFoundryLocalLanguage(normalizeFoundryLanguage(storedSettings.foundryLocalLanguage));
        }
        void checkAzureCliSignIn(
          storedSettings?.azureTenantId?.trim() || undefined,
          storedSettings?.azureSubscriptionId?.trim() || undefined,
        )
          .then((status) => setAzureCliStatus(status))
          .catch(() => undefined);
        const devices = await listNativeAudioInputDevices();
        setAudioInputDevices(devices.length > 0 ? devices : [{ id: '', name: 'System default', isDefault: true }]);
        const outputDevices = await listNativeAudioOutputDevices();
        setAudioOutputDevices(outputDevices.length > 0 ? outputDevices : [{ id: '', name: 'System default', isDefault: true }]);
        const runtimeStatus = await getRecordingRuntimeStatus();
        if (disposed) return;
        if (runtimeStatus.audioRecordingActive && runtimeStatus.audioMeetingId) {
          const activeRecording = storedMeetings.find((meeting) => meeting.id === runtimeStatus.audioMeetingId);
          const restoredElapsed = Math.floor(
            (activeRecording?.durationSeconds ?? 0) + runtimeStatus.audioElapsedSeconds
          );
          activeMeetingIdRef.current = runtimeStatus.audioMeetingId;
          recordingMeetingIdRef.current = runtimeStatus.audioMeetingId;
          setActiveMeetingId(runtimeStatus.audioMeetingId);
          setElapsedSeconds(restoredElapsed);
          elapsedSecondsRef.current = restoredElapsed;
          latestMicrophoneMuteRef.current = {
            meetingId: runtimeStatus.audioMeetingId,
            muted: runtimeStatus.audioMicrophoneMuted,
          };
          setMicrophoneMuted(runtimeStatus.audioMicrophoneMuted);
          setStatus('recording');
          if (!elapsedTimerRef.current) startTimers();
          identifyRecordingAttempt(attempt, runtimeStatus.audioMeetingId);

          if (!attempt.failure && activeRecording?.transcriptionEngine === 'azure' && storedSettings?.azureEndpoint?.trim()) {
            try {
              const meetingId = runtimeStatus.audioMeetingId;
              const pipe = await listenForAzureAudio(meetingId);
              if (disposed) return;
              if (!attempt.failure) {
                pipe.session = await createAzureSession(meetingId, activeRecording.language || storedSettings.azureLanguage || 'en-US', {
                  endpoint: storedSettings.azureEndpoint.trim(),
                  tenantId: storedSettings.azureTenantId?.trim() || undefined,
                  subscriptionId: storedSettings.azureSubscriptionId?.trim() || undefined,
                });
              }
              if (disposed) await closeAzurePipe(pipe);
              else if (!attempt.failure) flushAzureChunks(pipe);
            } catch (azureError) {
              setError(getErrorMessage(azureError, 'Recording restored, but Azure live transcription could not reconnect.'));
            }
          } else if (!attempt.failure && activeRecording?.transcriptionEngine === 'azure') {
            await listenForAzureAudio(runtimeStatus.audioMeetingId);
            setError('Recording restored locally, but Azure transcription needs a Speech endpoint. Stop and configure Azure, then retranscribe the saved audio.');
          }
        } else if (recordingAttemptRef.current === attempt) {
          recordingAttemptRef.current = null;
        }
        if (disposed) return;
        if (runtimeStatus.screenRecordingActive && runtimeStatus.screenVideoId) {
          activeScreenVideoIdRef.current = runtimeStatus.screenVideoId;
          setScreenElapsedSeconds(Math.floor(runtimeStatus.screenElapsedSeconds));
          setScreenStatus('recording');
          if (!screenElapsedTimerRef.current) {
            screenElapsedTimerRef.current = window.setInterval(
              () => setScreenElapsedSeconds((seconds) => seconds + 1),
              1000,
            );
          }
        }
        listFoundryLocalModels()
          .then((foundryModels) => {
            setFoundryLocalModels(foundryModels);
            if (!storedSettings?.foundryLocalModelAlias && foundryModels.length > 0) {
              setFoundryLocalModelAlias(foundryModels.find((model) => model.cached)?.alias ?? foundryModels[0].alias);
            }
          })
          .catch(() => undefined);
      } catch (loadError) {
        setError(loadError instanceof Error ? loadError.message : 'Tauri backend is not available.');
      } finally {
        if (!disposed) settingsLoadedRef.current = true;
      }
    };

    void enqueueRecordingOperation(load);

    const cleanups = new Set<() => void>();
    const registrations: Promise<void>[] = [];
    const registerListener = (listener: Promise<() => void>) => {
      const registration = listener
        .then((cleanup) => {
          if (disposed) {
            cleanup();
          } else {
            cleanups.add(cleanup);
          }
        })
        .catch((listenerError) => {
          if (!disposed) {
            setError(listenerError instanceof Error ? listenerError.message : 'Failed to register a native event listener.');
          }
          throw listenerError;
        });
      registrations.push(registration);
    };
    registerListener(
      onTranscriptSegment(({ meetingId, segment }) => {
        if (disposed) {
          return;
        }
        setMeetings((current) => {
          const target = current.find((meeting) => meeting.id === meetingId);
          if (!target || target.transcript.some((item) => item.id === segment.id)) {
            return current;
          }
          return upsertMeeting(current, {
            ...target,
            transcript: [...target.transcript, segment],
            updatedAt: new Date().toISOString(),
          });
        });
      }),
    );
    registerListener(
      onFoundryDownloadProgress((progress) => {
        if (disposed) {
          return;
        }
        setFoundryDownloadProgress(progress);
        if (progress.phase === 'done' || progress.phase === 'error') {
          setFoundryDownloadingAlias(null);
        }
      }),
    );
    registerListener(
      onMicrophoneMuteChanged(({ meetingId, muted }) => {
        latestMicrophoneMuteRef.current = { meetingId, muted };
        if (!disposed && meetingId === recordingMeetingIdRef.current) {
          setMicrophoneMuted(muted);
        }
      }),
    );
    registerListener(
      onCallMuteWarning(({ message }) => {
        if (!disposed) {
          setCallMuteWarning(message);
        }
      }),
    );
    registerListener(onRecordingError((event) => {
      if (!disposed) receiveRecordingFailure(event);
    }));
    registerListener(onTranscriptionError((message) => {
      if (!disposed) setError(message);
    }));
    recordingListenersReadyRef.current = Promise.all(registrations).then(() => undefined);
    // Start must still observe a registration failure even if no one records.
    void recordingListenersReadyRef.current.catch(() => undefined);

    return () => {
      disposed = true;
      cleanups.forEach((cleanup) => cleanup());
      cleanups.clear();
    };
  }, []);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void onAreaSelected((target) => {
      setSelectedScreenTarget(target);
      setError(null);
    }).then((cleanup) => {
      unlisten = cleanup;
    });
    return () => unlisten?.();
  }, []);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void onScreenAudioLevel(setScreenAudioLevels).then((cleanup) => {
      unlisten = cleanup;
    });
    return () => unlisten?.();
  }, []);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void onScreenProcessingStatus(setScreenProcessingStatus).then((cleanup) => {
      unlisten = cleanup;
    });
    return () => unlisten?.();
  }, []);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void onFastTranscriptionProgress(setFastTranscriptionProgress).then((cleanup) => {
      unlisten = cleanup;
    });
    return () => unlisten?.();
  }, []);

  useEffect(() => {
    activeMeetingIdRef.current = activeMeetingId;
  }, [activeMeetingId]);

  useEffect(() => {
    revokeFastConsent();
  }, [fastTranscriptionFile, activeMeetingId, retranscribeSourceId]);

  // Persist settings whenever they change. Debounced so per-keystroke edits in
  // the Azure fields do not rewrite the store on every character. Skipped until
  // the initial load has applied any stored values, so defaults never clobber
  // saved settings on startup.
  useEffect(() => {
    if (!settingsLoadedRef.current) {
      return;
    }
    const handle = window.setTimeout(() => {
      void saveNativeSettings(settingsSnapshot()).catch((settingsError) => {
        setError(getErrorMessage(settingsError, 'Could not save settings.'));
      });
    }, 400);
    return () => window.clearTimeout(handle);
  }, [
    transcriptionEngine,
    selectedCaptureMode,
    screenAudioCaptureMode,
    selectedAudioInputId,
    selectedSystemAudioOutputId,
    selectedLanguage,
    azureEndpoint,
    azureTenantId,
    azureSubscriptionId,
    azureLanguage,
    foundryLocalModelAlias,
    foundryLocalChunkingMode,
    foundryLocalLanguage,
    videoOutputFolder,
    videoCodec,
    ffmpegPath,
  ]);

  useEffect(() => {
    const element = transcriptListRef.current;
    if (element) {
      element.scrollTop = element.scrollHeight;
    }
  }, [activeMeetingId, activeMeeting?.transcript.length]);

  useEffect(() => {
    return () => {
      if (elapsedTimerRef.current) {
        window.clearInterval(elapsedTimerRef.current);
      }
      if (screenElapsedTimerRef.current) window.clearInterval(screenElapsedTimerRef.current);
      // Cleanup follows any in-flight setup; never close a replacement session
      // from a stale async completion (including React StrictMode remounts).
      void enqueueRecordingOperation(async () => {
        stopTimers();
        await closeAzurePipe(azurePipeRef.current);
      }).catch(() => undefined);
    };
  }, []);

  const updateActiveMeeting = (updater: (meeting: Meeting) => Meeting) => {
    setMeetings((current) => {
      const target = current.find((meeting) => meeting.id === activeMeetingIdRef.current);
      if (!target) {
        return current;
      }
      return upsertMeeting(current, updater(target));
    });
  };

  const tenantForAzureCli = () => azureTenantId.trim() || undefined;
  const subscriptionForAzureCli = () => azureSubscriptionId.trim() || undefined;

  const handleAzureSignIn = async () => {
    setError(null);
    setAzureSigningIn(true);
    try {
      const status = await signInAzureCli(tenantForAzureCli(), subscriptionForAzureCli());
      setAzureCliStatus(status);
      if (status.state === 'error') {
        setError(status.detail);
        return false;
      }
      return true;
    } catch (authError) {
      setError(getErrorMessage(authError, 'Azure CLI sign-in failed.'));
      return false;
    } finally {
      setAzureSigningIn(false);
    }
  };

  const ensureAzureCliSignedIn = async () => {
    const status = await checkAzureCliSignIn(tenantForAzureCli(), subscriptionForAzureCli());
    setAzureCliStatus(status);
    if (status.state === 'connected') {
      return true;
    }
    return handleAzureSignIn();
  };

  const startTimers = () => {
    elapsedTimerRef.current = window.setInterval(() => {
      setElapsedSeconds((value) => {
        const next = value + 1;
        elapsedSecondsRef.current = next;
        return next;
      });
      const meetingId = recordingMeetingIdRef.current;
      setMeetings((current) => current.map((meeting) => meeting.id === meetingId ? {
        ...meeting,
        durationSeconds: meeting.durationSeconds + 1,
        updatedAt: new Date().toISOString(),
      } : meeting));
    }, 1000);
  };

  const stopTimers = () => {
    if (elapsedTimerRef.current) {
      window.clearInterval(elapsedTimerRef.current);
      elapsedTimerRef.current = null;
    }
  };

  const handleStartRecording = (appendToMeetingId?: string): Promise<void> => {
    // Acquire synchronously: React state alone cannot guard rapid double clicks.
    if (recordingOperationsRef.current || recordingMeetingIdRef.current || fastTranscribingRef.current) return Promise.resolve();
    setStatus('starting');
    return enqueueRecordingOperation(async () => {
      setError(null);
      setShowRecordingList(false);
      setAzurePartialText('');
      setMicrophoneMuted(false);
      setCallMuteWarning(null);
      const existing = meetings.find((meeting) => meeting.id === appendToMeetingId);
      // Continue the meeting's live engine; file-transcribed meetings use Azure
      // live recognition when appended, not an unrelated global default.
      const engine = existing
        ? existing.transcriptionEngine?.startsWith('azure') ? 'azure' : 'foundryLocal'
        : transcriptionEngine;
      const language = engine === 'azure'
        ? normalizeAzureLanguage(existing?.language || azureLanguage)
        : normalizeFoundryLanguage(existing?.language || foundryLocalLanguage || selectedLanguage);
      let pipe: AzureRecordingPipe | null = null;
      const attempt = beginRecordingAttempt();
      try {
        await recordingListenersReadyRef.current;
        if (appendToMeetingId && !existing) throw new Error('The meeting to continue is no longer available.');
        if (engine === 'azure') {
          if (!azureEndpoint.trim()) throw new Error('Enter the Speech custom domain endpoint before recording with Azure.');
          if (!await ensureAzureCliSignedIn()) throw new Error('Azure CLI sign-in is required before recording with Azure Speech.');
          // Subscribe before capture starts. New recordings have no ID until IPC
          // returns, so keep early chunks and filter them once the ID is known.
          pipe = await listenForAzureAudio(appendToMeetingId ?? null);
        } else if (!foundryLocalModelAlias.trim()) {
          throw new Error('Select or download a Foundry Local speech model before recording.');
        }
        const meeting = await startNativeRecording({
          appendToMeetingId,
          audioDeviceId: selectedAudioInputId,
          systemAudioDeviceId: selectedSystemAudioOutputId,
          captureMode: selectedCaptureMode,
          language,
          transcriptionEngine: engine,
          foundryLocalModelAlias,
          foundryLocalChunkingMode,
        });
        recordingMeetingIdRef.current = meeting.id;
        setMeetings((current) => upsertMeeting(current, meeting));
        setActiveMeetingId(meeting.id);
        activeMeetingIdRef.current = meeting.id;
        const latestMute = latestMicrophoneMuteRef.current;
        setMicrophoneMuted(latestMute?.meetingId === meeting.id ? latestMute.muted : false);
        setElapsedSeconds(meeting.durationSeconds);
        elapsedSecondsRef.current = meeting.durationSeconds;
        startTimers();
        identifyRecordingAttempt(attempt, meeting.id);
        if (attempt.failure) return;
        if (pipe) {
          pipe.meetingId = meeting.id;
          pipe.session = await createAzureSession(meeting.id, language, {
            endpoint: azureEndpoint.trim(), tenantId: tenantForAzureCli(), subscriptionId: subscriptionForAzureCli(),
          });
          if (!attempt.failure) flushAzureChunks(pipe);
        }
        if (!attempt.failure) setStatus('recording');
      } catch (recordingError) {
        const message = getErrorMessage(recordingError, 'Native recording could not start.');
        if (attempt.failure) {
          // Normal stop is already queued. Azure startup may fail concurrently;
          // do not issue a second native stop from this startup error handler.
          setError(message);
          surfaceRecordingFailure(attempt);
          return;
        }
        // Startup cleanup must use the same serialized stop as capture failures
        // and user Stop. Never await it here: it runs after startup settles.
        if (recordingMeetingIdRef.current) {
          setError(message);
          void handleStopRecording(attempt).catch(() => surfaceRecordingFailure(attempt));
          return;
        }
        if (!recordingMeetingIdRef.current) {
          stopTimers();
          await closeAzurePipe(pipe).catch(() => undefined);
        }
        setStatus(recordingMeetingIdRef.current ? 'recording' : 'idle');
        setMicrophoneMuted(false);
        setError(message);
      } finally {
        if (!recordingMeetingIdRef.current && recordingAttemptRef.current === attempt && !attempt.failure) {
          recordingAttemptRef.current = null;
        }
      }
    });
  };

  const handleOpenRecordingChoice = () => {
    if (recordingOperationsRef.current || recordingMeetingIdRef.current || fastTranscribingRef.current) return;
    setError(null);
    setShowRecordingChoice(true);
  };

  const handleStartNewRecording = () => {
    setShowRecordingChoice(false);
    void handleStartRecording();
  };

  const handleAppendToOpenMeeting = () => {
    if (!activeMeeting) {
      return;
    }
    setShowRecordingChoice(false);
    void handleStartRecording(activeMeeting.id);
  };

  const handleSelectFastTranscriptionFile = async () => {
    if (fastTranscribingRef.current) return;
    revokeFastConsent();
    setError(null);
    try {
      const file = await selectFastTranscriptionAudio();
      if (file) {
        setFastTranscriptionFile(file);
        setRetranscribeSourceId(null);
        setFastLanguage(normalizeAzureLanguage(azureLanguage));
        setFastTranscriptionProgress(null);
      }
    } catch (selectionError) {
      setError(getErrorMessage(selectionError, 'Could not select an audio file.'));
    }
  };

  const transcribeFile = async (file: FastTranscriptionFile): Promise<boolean> => {
    if (fastTranscribingRef.current || recordingOperationsRef.current || recordingMeetingIdRef.current || screenStatus !== 'idle') {
      return false;
    }
    const consent = fastConsentRef.current;
    if (!fastUploadConsent || file !== fastTranscriptionFile || !consent || consent.file !== file || consent.meetingId !== activeMeetingId) {
      setError('Review this file and explicitly consent to uploading its audio to Azure Speech.');
      return false;
    }
    fastTranscribingRef.current = true;
    setError(null);
    setFastTranscribing(true);
    setFastTranscriptionProgress({ phase: 'validating', detail: 'Validating the selected media file.' });
    try {
      if (!azureEndpoint.trim()) {
        throw new Error('Enter the Speech custom domain endpoint before starting file transcription.');
      }
      const signedIn = await ensureAzureCliSignedIn();
      if (!signedIn) {
        throw new Error('Azure CLI sign-in is required before file transcription.');
      }
      if (fastConsentRef.current !== consent) {
        throw new Error('The file or meeting changed while signing in. Review the file and give upload consent again.');
      }
      const meeting = await transcribeFastAudio(
        file.path,
        file.name.replace(/\.(wav|mp3|mp4)$/i, ''),
        normalizeAzureLanguage(fastLanguage),
        azureEndpoint,
        tenantForAzureCli(),
        subscriptionForAzureCli(),
        true,
        fastDiarization,
      );
      setMeetings((current) => upsertMeeting(current, meeting));
      setActiveMeetingId(meeting.id);
      setFastTranscriptionProgress({ phase: 'saving', detail: 'Transcription saved locally.' });
      return true;
    } catch (transcriptionError) {
      setError(getErrorMessage(transcriptionError, 'Azure Speech file transcription failed.'));
      return false;
    } finally {
      fastTranscribingRef.current = false;
      setFastTranscribing(false);
      revokeFastConsent();
    }
  };

  const handleStartFastTranscription = async () => {
    if (!fastTranscriptionFile) {
      return;
    }
    if (await transcribeFile(fastTranscriptionFile)) {
      setShowBatchTranscription(false);
      setShowRecordingList(false);
      setFastTranscriptionFile(null);
      setRetranscribeSourceId(null);
    }
  };

  const handleStopRecording = (expectedAttempt = recordingAttemptRef.current): Promise<void> => {
    if (expectedAttempt !== recordingAttemptRef.current) return Promise.resolve();
    if (stopRecordingPromiseRef.current) {
      return stopRecordingPromiseRef.current;
    }
    const operation = enqueueRecordingOperation(async () => {
      if (expectedAttempt !== recordingAttemptRef.current || !recordingMeetingIdRef.current) {
        if (expectedAttempt) surfaceRecordingFailure(expectedAttempt);
        return;
      }
      setStatus('saving');
      stopTimers();
      try {
        const meeting = await stopNativeRecording();
        recordingMeetingIdRef.current = null;
        // Native stop waits for the last PCM emission. Only now remove the
        // listener and close the stream; stop also awaits final transcript writes.
        const pipe = azurePipeRef.current;
        if (pipe?.chunks.length) setError('Some buffered audio could not be transcribed live. The saved audio can be retranscribed.');
        await closeAzurePipe(pipe);
        setAzurePartialText('');
        setMicrophoneMuted(false);
        setMeetings((current) => upsertMeeting(current, meeting));
        // Native's stop result predates the recognizer's final persisted text.
        const persisted = await fetchMeetings();
        const finalMeeting = persisted.find((item) => item.id === meeting.id);
        if (finalMeeting) setMeetings((current) => upsertMeeting(current, finalMeeting));
        setStatus('idle');
      } catch (stopError) {
        let message = stopError instanceof Error ? stopError.message : 'Failed to stop recording.';
        // A native finalization error can occur AFTER capture has stopped.
        // Conversely, never detach live PCM if capture really is still active.
        if (recordingMeetingIdRef.current) {
          try {
            const runtime = await getRecordingRuntimeStatus();
            if (!runtime.audioRecordingActive) recordingMeetingIdRef.current = null;
          } catch {
            message += ' Could not confirm capture stopped; retry Stop.';
          }
        }
        if (!recordingMeetingIdRef.current || expectedAttempt?.failure) {
          try {
            await closeAzurePipe(azurePipeRef.current);
          } catch (azureStopError) {
            message = `${message} ${getErrorMessage(azureStopError, 'Azure Speech could not stop cleanly.')}`;
          }
        } else {
          startTimers();
        }
        setAzurePartialText('');
        setMicrophoneMuted(false);
        setError(message);
        setStatus(recordingMeetingIdRef.current ? 'recording' : 'idle');
        const persisted = await fetchMeetings().catch(() => []);
        setMeetings((current) => persisted.reduce(upsertMeeting, current));
      } finally {
        // Stop success and cleanup errors may both replace the UI error.
        // Always retain the capture failure and finalized local WAV path.
        if (expectedAttempt) surfaceRecordingFailure(expectedAttempt);
      }
    });
    stopRecordingPromiseRef.current = operation;
    void operation.finally(() => {
      if (stopRecordingPromiseRef.current === operation) {
        stopRecordingPromiseRef.current = null;
      }
    }).catch(() => undefined);
    return operation;
  };

  const handleSelectArea = async () => {
    try {
      await openAreaSelector();
    } catch (selectionError) {
      setError(getErrorMessage(selectionError, 'Could not open area selection.'));
    }
  };

  const handleStartScreenRecording = async () => {
    setError(null);
    setScreenProcessingStatus(null);
    setScreenStatus('starting');
    const target = selectedScreenTarget;
    if (!target) {
      setScreenStatus('idle');
      setError('No screen target is available.');
      return;
    }
    try {
      await saveNativeSettings(settingsSnapshot());
      const video = await startScreenRecording({
        targetId: target.id,
        targetName: target.name,
        x: target.x,
        y: target.y,
        width: target.width,
        height: target.height,
        codec: videoCodec,
        audioDeviceId: selectedAudioInputId,
        systemAudioDeviceId: selectedSystemAudioOutputId,
        captureMode: screenAudioCaptureMode,
      });
      activeScreenVideoIdRef.current = video.id;
      setVideos((current) => [video, ...current.filter((item) => item.id !== video.id)]);
      setScreenElapsedSeconds(0);
      setScreenAudioLevels({ microphone: 0, system: 0, mixed: 0 });
      setScreenStatus('recording');
      screenElapsedTimerRef.current = window.setInterval(() => setScreenElapsedSeconds((seconds) => seconds + 1), 1000);
      handleMiniModeChange(true, 'screen');
    } catch (screenError) {
      activeScreenVideoIdRef.current = null;
      setScreenStatus('idle');
      setError(getErrorMessage(screenError, 'Screen recording could not start.'));
    }
  };

  const handleStopScreenRecording = async () => {
    const activeVideoId = activeScreenVideoIdRef.current;
    setScreenProcessingStatus({ stage: 'Stopping capture', detail: 'Closing the screen video stream.' });
    setScreenStatus('saving');
    if (screenElapsedTimerRef.current) {
      window.clearInterval(screenElapsedTimerRef.current);
      screenElapsedTimerRef.current = null;
    }
    try {
      const video = await stopScreenRecording();
      activeScreenVideoIdRef.current = null;
      setVideos((current) => [video, ...current.filter((item) => item.id !== video.id)]);
      setScreenStatus('idle');
      setScreenAudioLevels({ microphone: 0, system: 0, mixed: 0 });
      setScreenProcessingStatus(null);
    } catch (screenError) {
      activeScreenVideoIdRef.current = null;
      setScreenStatus('idle');
      setScreenAudioLevels({ microphone: 0, system: 0, mixed: 0 });
      const message = getErrorMessage(screenError, 'Screen recording could not stop.');
      try {
        const refreshed = await refreshVideos();
        setVideos(refreshed);
        const failedVideo = refreshed.find((video) => video.id === activeVideoId);
        if (failedVideo?.status === 'saved') {
          const audioFailed = failedVideo.postProcessStage === 'audio-capture-failed';
          setScreenProcessingStatus(audioFailed ? {
            stage: 'Video saved without audio',
            detail: 'The video is valid, but screen audio capture failed. The staged WAV was preserved.',
          } : {
            stage: 'Recording saved',
            detail: 'Post-processing completed during recovery.',
          });
          setError(audioFailed ? message : null);
        } else if (failedVideo?.status === 'capture-failed') {
          setScreenProcessingStatus({
            stage: 'Capture failed',
            detail: 'FFmpeg did not encode any video frames. Select the area again and retry.',
          });
          setError(message);
        } else {
          setScreenProcessingStatus({
            stage: 'Recovery pending',
            detail: 'The recording files were kept and will be retried when you refresh or reopen the app.',
          });
          setError(message);
        }
      } catch {
        setScreenProcessingStatus({
          stage: 'Recovery pending',
          detail: 'The recording files were kept and will be retried when you refresh or reopen the app.',
        });
        setError(message);
      }
    }
  };

  const handleChooseVideoOutputFolder = async () => {
    try {
      const folder = await selectVideoOutputFolder();
      if (folder) {
        await saveNativeSettings(settingsSnapshot({ videoOutputFolder: folder }));
        setVideoOutputFolder(folder);
      }
    } catch (folderError) {
      setError(getErrorMessage(folderError, 'Could not choose a video output folder.'));
    }
  };

  const handleChooseFfmpegExecutable = async () => {
    try {
      const executable = await selectFfmpegExecutable();
      if (executable) {
        await saveNativeSettings(settingsSnapshot({ ffmpegPath: executable }));
        setFfmpegPath(executable);
      }
    } catch (selectionError) {
      setError(getErrorMessage(selectionError, 'Could not select an FFmpeg executable.'));
    }
  };

  const handleUseBundledFfmpeg = async () => {
    try {
      await saveNativeSettings(settingsSnapshot({ ffmpegPath: '' }));
      setFfmpegPath('');
    } catch (settingsError) {
      setError(getErrorMessage(settingsError, 'Could not switch to the bundled FFmpeg runtime.'));
    }
  };

  const handleOpenVideoRecordingsFolder = async () => {
    try {
      await openVideoRecordingsFolder();
    } catch (folderError) {
      setError(getErrorMessage(folderError, 'Could not open the recordings folder.'));
    }
  };

  const handleRefreshMeetings = async () => {
    setRefreshingMeetings(true);
    try {
      setMeetings(await refreshMeetings());
      setError(null);
    } catch (refreshError) {
      setError(getErrorMessage(refreshError, 'Could not refresh audio recordings.'));
    } finally {
      setRefreshingMeetings(false);
    }
  };

  const handleRefreshVideos = async () => {
    setRefreshingVideos(true);
    try {
      setVideos(await refreshVideos());
      setError(null);
    } catch (refreshError) {
      setError(getErrorMessage(refreshError, 'Could not refresh screen recordings.'));
    } finally {
      setRefreshingVideos(false);
    }
  };

  const handleGenerateVideoTranscription = async (video: VideoRecording) => {
    if (video.hasAudio === false) {
      setError('This screen recording does not contain an audio track to transcribe.');
      return;
    }
    if (fastTranscribingRef.current) return;
    revokeFastConsent();
    setRetranscribeSourceId(null);
    setFastLanguage(normalizeAzureLanguage(azureLanguage));
    setFastTranscriptionProgress(null);
    setFastTranscriptionFile({
      path: video.videoPath,
      name: video.title,
      sizeBytes: 0,
      durationSeconds: video.durationSeconds,
      requiresMp3Extraction: true,
    });
    setShowBatchTranscription(true);
    setShowVideoWorkspace(false);
    setShowRecordingList(false);
  };

  const handleRetranscribeMeeting = (meeting: Meeting) => {
    if (!meeting.recordingPath || status !== 'idle' || recordingOperationsRef.current || fastTranscribingRef.current) return;
    revokeFastConsent();
    setFastTranscriptionProgress(null);
    setRetranscribeSourceId(meeting.id);
    setFastLanguage(normalizeAzureLanguage(meeting.language));
    setFastTranscriptionFile({
      path: meeting.recordingPath,
      name: `${meeting.title} (retranscribed)`,
      sizeBytes: 0, // Backend probes and validates the actual saved file.
      durationSeconds: meeting.durationSeconds,
      requiresMp3Extraction: /\.mp4$/i.test(meeting.recordingPath),
    });
    setShowBatchTranscription(true);
    setShowVideoWorkspace(false);
    setShowSettings(false);
    setShowRecordingList(false);
    setError(null);
  };

  const handleDeleteVideo = async (videoId: string) => {
    try {
      await deleteNativeVideo(videoId);
      setVideos((current) => current.filter((video) => video.id !== videoId));
      setError(null);
    } catch (deleteError) {
      setError(getErrorMessage(deleteError, 'Could not remove the video recording.'));
    }
  };

  const handleRenameVideo = async (videoId: string, title: string) => {
    try {
      const video = await renameNativeVideo(videoId, title);
      setVideos((current) => current.map((item) => item.id === video.id ? video : item));
      setError(null);
    } catch (renameError) {
      setError(getErrorMessage(renameError, 'Could not rename the video recording.'));
    }
  };

  const handleSelectMeeting = (meetingId: string) => {
    if (meetingId === activeMeetingId) {
      return;
    }
    revokeFastConsent();
    if (status !== 'idle' || recordingMeetingIdRef.current || recordingOperationsRef.current) {
      setPendingMeetingId(meetingId);
      return;
    }
    setActiveMeetingId(meetingId);
  };

  const handleCancelSwitch = () => {
    setPendingMeetingId(null);
  };

  const handleConfirmSwitch = async () => {
    const targetId = pendingMeetingId;
    setPendingMeetingId(null);
    if (!targetId) {
      return;
    }
    await handleStopRecording();
    if (!recordingMeetingIdRef.current) setActiveMeetingId(targetId);
  };

  const handleExportAudio = async (meeting: Meeting) => {
    try {
      await exportNativeAudio(meeting.id);
      setError(null);
    } catch (audioError) {
      setError(audioError instanceof Error ? audioError.message : 'No audio file is available for this meeting yet.');
    }
  };

  const handleExportTranscript = async (meeting: Meeting) => {
    try {
      await exportNativeTranscript(meeting.id);
      setError(null);
    } catch (transcriptError) {
      setError(transcriptError instanceof Error ? transcriptError.message : 'Failed to export transcript.');
    }
  };

  const handleOpenMeetingFolder = async (meeting: Meeting) => {
    try {
      await openNativeMeetingFolder(meeting.id);
      setError(null);
    } catch (folderError) {
      setError(folderError instanceof Error ? folderError.message : 'Failed to open local folder.');
    }
  };

  const markPlaybackActive = (meetingId: string, segmentId: string | null) => {
    setPlayingMeetingId(meetingId);
    setPlayingSegmentId(segmentId);
    setPlaybackPaused(false);
  };

  const clearPlaybackState = () => {
    setPlayingMeetingId(null);
    setPlayingSegmentId(null);
    setPlaybackPaused(false);
  };

  const handleTogglePlayback = async (meeting: Meeting) => {
    try {
      if (playingMeetingId !== meeting.id) {
        await playNativeRecording(meeting.id);
        markPlaybackActive(meeting.id, null);
        setError(null);
        return;
      }

      if (playbackPaused) {
        await resumeNativePlayback();
        setPlaybackPaused(false);
      } else {
        await pauseNativePlayback();
        setPlaybackPaused(true);
      }
      setError(null);
    } catch (playbackError) {
      setError(playbackError instanceof Error ? playbackError.message : 'Failed to control playback.');
    }
  };

  const handlePlaySegment = async (meeting: Meeting, segment: TranscriptSegment) => {
    try {
      await playNativeRecording(meeting.id, segment.offsetSeconds);
      markPlaybackActive(meeting.id, segment.id);
      setError(null);
    } catch (playbackError) {
      setError(playbackError instanceof Error ? playbackError.message : 'Failed to play from this point.');
    }
  };

  const handleAssistantSeek = async (meeting: Meeting, offsetSeconds: number) => {
    try {
      if (!meeting.hasAudio) throw new Error('No saved audio is available for this source.');
      await playNativeRecording(meeting.id, offsetSeconds);
      const segment = meeting.transcript.find((item) => item.offsetSeconds === offsetSeconds);
      markPlaybackActive(meeting.id, segment?.id ?? null);
      setError(null);
    } catch (playbackError) {
      setError(getErrorMessage(playbackError, 'Failed to play the assistant source.'));
    }
  };

  const handleRenameSpeaker = async (meeting: Meeting, speakerId: string, name: string) => {
    try {
      const updated = await renameNativeSpeaker(meeting.id, speakerId, name.trim());
      const renamed = updated.transcript.find((segment) => segment.speakerId === speakerId);
      setMeetings((current) => current.map((item) => item.id === meeting.id ? {
        ...item,
        updatedAt: updated.updatedAt,
        transcript: item.transcript.map((segment) => segment.speakerId === speakerId
          ? { ...segment, speakerName: renamed?.speakerName } : segment),
      } : item));
      setError(null);
    } catch (renameError) {
      setError(getErrorMessage(renameError, 'Could not save the speaker name.'));
    }
  };

  const handleDeleteMeeting = async (meetingId: string) => {
    await deleteNativeMeeting(meetingId);
    setMeetings((current) => current.filter((meeting) => meeting.id !== meetingId));
    if (playingMeetingId === meetingId) {
      clearPlaybackState();
    }
    if (activeMeetingId === meetingId) {
      const remaining = meetings.filter((meeting) => meeting.id !== meetingId);
      setActiveMeetingId(remaining[0]?.id ?? null);
    }
  };

  const handleRefreshFoundryModels = async () => {
    setFoundryLoadingModels(true);
    setError(null);
    try {
      const models = await listFoundryLocalModels();
      setFoundryLocalModels(models);
      if (!models.some((model) => model.alias === foundryLocalModelAlias)) {
        setFoundryLocalModelAlias(models.find((model) => model.cached)?.alias ?? models[0]?.alias ?? '');
      }
    } catch (foundryError) {
      setError(getErrorMessage(foundryError, 'Failed to load Foundry Local models.'));
    } finally {
      setFoundryLoadingModels(false);
    }
  };

  const handleDownloadFoundryModel = async () => {
    const alias = foundryLocalModelAlias.trim();
    if (!alias) {
      setError('Select a Foundry Local speech model first.');
      return;
    }
    setError(null);
    setFoundryDownloadingAlias(alias);
    setFoundryDownloadProgress({ alias, phase: 'model', percent: 0 });
    try {
      await downloadFoundryLocalModel(alias);
      await handleRefreshFoundryModels();
    } catch (foundryError) {
      setFoundryDownloadProgress({
        alias,
        phase: 'error',
        percent: 0,
        message: getErrorMessage(foundryError, 'Foundry Local model download failed.'),
      });
      setError(getErrorMessage(foundryError, 'Foundry Local model download failed.'));
    } finally {
      setFoundryDownloadingAlias(null);
    }
  };

  const handleRefreshAudioInputs = async () => {
    try {
      const devices = await listNativeAudioInputDevices();
      const nextDevices = devices.length > 0 ? devices : [{ id: '', name: 'System default', isDefault: true }];
      setAudioInputDevices(nextDevices);
      if (!nextDevices.some((device) => device.id === selectedAudioInputId)) {
        setSelectedAudioInputId('');
      }
      const outputDevices = await listNativeAudioOutputDevices();
      const nextOutputDevices = outputDevices.length > 0 ? outputDevices : [{ id: '', name: 'System default', isDefault: true }];
      setAudioOutputDevices(nextOutputDevices);
      if (!nextOutputDevices.some((device) => device.id === selectedSystemAudioOutputId)) {
        setSelectedSystemAudioOutputId('');
      }
      setError(null);
    } catch (deviceError) {
      setError(deviceError instanceof Error ? deviceError.message : 'Failed to load audio inputs.');
    }
  };

  const handleSessionLanguageChange = (meeting: Meeting, value: string) => enqueueRecordingOperation(async () => {
    const attempt = recordingAttemptRef.current;
    if (attempt?.failure && attempt.meetingId === meeting.id) {
      surfaceRecordingFailure(attempt);
      return;
    }
    const language = meeting.transcriptionEngine?.startsWith('azure')
      ? normalizeAzureLanguage(value) : normalizeFoundryLanguage(value);
    let pipe: AzureRecordingPipe | null = null;
    let previousSession: AzureSpeechSession | null = null;
    let replacement: AzureSpeechSession | null = null;
    let committed = false;
    setError(null);
    try {
      if (meeting.transcriptionEngine === 'azure' && recordingMeetingIdRef.current === meeting.id) {
        pipe = azurePipeRef.current ?? await listenForAzureAudio(meeting.id);
        if (attempt?.failure) return;
        previousSession = pipe.session;
        // Do not send transition audio into a recognizer that is stopping.
        // The listener remains attached and buffers until the replacement is ready.
        pipe.session = null;
        replacement = await createAzureSession(meeting.id, language, {
          endpoint: azureEndpoint.trim(), tenantId: tenantForAzureCli(), subscriptionId: subscriptionForAzureCli(),
        });
      }
      const updatedMeeting = await updateNativeMeetingLanguage(meeting.id, language);
      committed = true;
      // Metadata only for saved meetings. Foundry applies live changes at its
      // utterance boundaries; neither path mutates the user's global defaults.
      setMeetings((current) => current.map((item) => item.id === meeting.id
        ? { ...item, language: updatedMeeting.language, updatedAt: updatedMeeting.updatedAt } : item));
      if (pipe) {
        await previousSession?.stop();
        pipe.session = replacement;
        flushAzureChunks(pipe);
      }
      setAzurePartialText('');
    } catch (languageError) {
      if (pipe) {
        if (committed) {
          pipe.session = replacement;
        } else {
          await replacement?.stop().catch(() => undefined);
          pipe.session = previousSession;
        }
        try { flushAzureChunks(pipe); } catch { /* Report the original failure below. */ }
      }
      setError(getErrorMessage(languageError, 'Failed to change the transcription language.'));
    }
  });

  const handleMiniModeChange = (
    enabled: boolean,
    target: 'transcription' | 'screen' = showVideoWorkspace ? 'screen' : 'transcription',
  ) => {
    if (enabled) {
      setMiniModeTarget(target);
    }
    setMiniMode(enabled);
    setNativeMiniMode(enabled).catch((modeError) => {
      setError(modeError instanceof Error ? modeError.message : 'Failed to resize the app window.');
    });
  };

  if (miniMode) {
    const screenMiniMode = miniModeTarget === 'screen';
    const screenAudioSources = [
      { label: 'System', level: screenAudioLevels.system, active: screenAudioCaptureMode !== 'microphone' && screenAudioCaptureMode !== 'none', tone: 'system' },
      { label: 'Microphone', level: screenAudioLevels.microphone, active: screenAudioCaptureMode !== 'system' && screenAudioCaptureMode !== 'none', tone: 'microphone' },
      { label: 'Mix', level: screenAudioLevels.mixed, active: screenAudioCaptureMode !== 'none', tone: 'mix' },
    ];
    return (
      <div className="mini-shell">
        <header className="mini-topbar">
          {screenMiniMode ? (
            <div className="mini-status-group">
              <Monitor size={16} />
              <RecorderStatusDisplay
                className="mini-recording-state"
                status={screenStatus}
                elapsedSeconds={screenElapsedSeconds}
              />
            </div>
          ) : (
            <RecorderStatusDisplay
              className="mini-status-group"
              status={status}
              elapsedSeconds={elapsedSeconds}
              microphoneMuted={microphoneMuted}
            />
          )}
          <div className="mini-actions">
            {screenMiniMode ? (
              <button
                type="button"
                className="mini-record"
                onClick={screenStatus === 'idle' ? handleStartScreenRecording : handleStopScreenRecording}
                disabled={screenStatus === 'starting' || screenStatus === 'saving'}
                title={screenStatus === 'idle' ? 'Start screen recording' : 'Stop screen recording'}
                aria-label={screenStatus === 'idle' ? 'Start screen recording' : 'Stop screen recording'}
              >
                {screenStatus === 'idle' ? <Video size={17} /> : <CircleStop size={17} />}
              </button>
            ) : (
              <RecordButton
                className="mini-record"
                status={status}
                iconSize={17}
                onStart={handleOpenRecordingChoice}
                onStop={handleStopRecording}
                title={status === 'idle' ? 'Start recording' : 'Stop recording'}
              />
            )}
            <button onClick={() => handleMiniModeChange(false)} title="Exit mini mode">
              <Maximize2 size={16} />
            </button>
          </div>
        </header>

        {screenMiniMode ? (
          <main className="mini-screen-recording" aria-label="Screen recording controls">
            <strong>{screenProcessingStatus?.stage ?? 'Screen recording'}</strong>
            {selectedScreenTarget && (
              <div className="mini-screen-target">
                <span>{selectedScreenTarget.name}</span>
                <span>{selectedScreenTarget.x}, {selectedScreenTarget.y} · {selectedScreenTarget.width} × {selectedScreenTarget.height} · {screenAudioCaptureMode}</span>
              </div>
            )}
            <div className="mini-screen-audio" aria-label="Screen recording audio mix">
              {screenAudioSources.map((source) => (
                <div className={`mini-screen-audio-source ${source.tone} ${source.active ? '' : 'inactive'}`} key={source.label}>
                  <span>{source.label}</span>
                  <div className="mini-screen-audio-bars" aria-label={`${source.label} level`}>
                    {Array.from({ length: 10 }, (_, index) => <i key={index} className={source.active && index < Math.ceil(source.level * 10) ? 'active' : ''} />)}
                  </div>
                </div>
              ))}
            </div>
            {screenProcessingStatus ? (
              <p className="mini-processing-status" role="status">{screenProcessingStatus.detail}</p>
            ) : screenStatus !== 'recording' && <p>Start recording with the capture target selected in the full window.</p>}
          </main>
        ) : (
          <main className="mini-transcript" ref={transcriptListRef} aria-live="polite">
            {activeMeeting?.transcript.length || azurePartialText ? (
              <>
              {activeMeeting?.transcript.map((segment) => (
                <div className="mini-transcript-row" key={segment.id}>
                  <span>{formatTimestamp(segment.offsetSeconds)}</span>
                  <p>{segment.speakerId && <strong className="speaker-badge" style={SPEAKER_BADGE_STYLE}>{segment.speakerName || segment.speakerId}</strong>}{segment.text}</p>
                </div>
              ))}
              {azurePartialText && (
                <div className="mini-transcript-row pending">
                  <span>live</span>
                  <p>{azurePartialText}</p>
                </div>
              )}
              </>
            ) : (
              <div className="mini-transcript-empty">
                <MessageSquareText size={15} />
                <p>Live transcript will appear here.</p>
              </div>
            )}
          </main>
        )}
      </div>
    );
  }

  return (
    <div className={`app-shell ${showRecordingList ? 'list-open' : ''} ${assistantExpanded && activeMeeting && !showVideoWorkspace && !showBatchTranscription ? 'assistant-open' : ''}`}>
      <aside className="rail" aria-label="Primary navigation">
        <button
          className={`rail-button ${showRecordingList ? 'active' : ''}`}
          title="Toggle recordings"
          onClick={() => {
            setShowRecordingList((value) => !value);
            setShowBatchTranscription(false);
            setShowVideoWorkspace(false);
          }}
        >
          <CalendarDays size={20} />
        </button>
        <button
          className={`rail-button video-rail-button ${showVideoWorkspace ? 'active' : ''}`}
          title="Screen recording"
          aria-label="Screen recording"
          onClick={() => {
            setShowVideoWorkspace((value) => !value);
            setShowRecordingList(false);
            setShowBatchTranscription(false);
            setShowSettings(false);
          }}
          disabled={status !== 'idle'}
        >
          <Monitor size={20} />
        </button>
        <button
          className={`rail-button ${showBatchTranscription ? 'active' : ''}`}
          title="Batch transcription"
          aria-label="Batch transcription"
          onClick={() => {
            revokeFastConsent();
            if (!fastTranscriptionFile) setFastLanguage(normalizeAzureLanguage(azureLanguage));
            setShowBatchTranscription((value) => !value);
            setShowRecordingList(false);
            setShowSettings(false);
            setShowVideoWorkspace(false);
          }}
          disabled={status !== 'idle'}
        >
          <FileAudio size={20} />
        </button>
      </aside>

      {showRecordingList && <aside className="meeting-list">
        <div className="product-name">Meetly Lite</div>
        <div className="search-box">
          <Search size={16} />
          <input
            value={searchQuery}
            onChange={(event) => setSearchQuery(event.target.value)}
            placeholder="Search meetings"
          />
        </div>

        <div className="meeting-list-heading">
          <div className="meeting-section-label">Recordings</div>
          <button type="button" className="meeting-list-refresh" onClick={handleRefreshMeetings} disabled={refreshingMeetings} title="Refresh audio recordings" aria-label="Refresh audio recordings">
            <RefreshCw size={15} className={refreshingMeetings ? 'spinning' : ''} />
          </button>
        </div>
        <div className="meeting-cards">
          {filteredMeetings.map((meeting) => (
            <button
              key={meeting.id}
              className={`meeting-card ${meeting.id === activeMeetingId ? 'selected' : ''}`}
              onClick={() => handleSelectMeeting(meeting.id)}
            >
              <span>{meeting.title}</span>
              <small>
                {new Date(meeting.createdAt).toLocaleDateString()} · {formatTimestamp(meeting.durationSeconds)}
              </small>
            </button>
          ))}
          {filteredMeetings.length === 0 && <div className="empty-list">No saved recordings</div>}
        </div>
      </aside>}

      <main className="workspace">
        <header className="topbar">
          <RecorderStatusDisplay
            className="recording-state"
            status={status}
            elapsedSeconds={elapsedSeconds}
            microphoneMuted={microphoneMuted}
          />
          <div className="top-actions">
            {activeMeeting && !showVideoWorkspace && !showBatchTranscription && <button type="button" className="text-action" aria-expanded={assistantExpanded} aria-controls="meeting-assistant-panel" onClick={() => setAssistantExpanded((expanded) => !expanded)}>
              Copilot · GHCP
            </button>}
            <button onClick={() => handleMiniModeChange(true)} title="Mini mode" aria-label="Mini mode">
              <Minimize2 size={16} />
            </button>
            <button onClick={() => {
              setShowSettings((value) => !value);
              setShowBatchTranscription(false);
              setShowVideoWorkspace(false);
            }} title="Settings" aria-label="Settings">
              <SlidersHorizontal size={16} />
            </button>
          </div>
        </header>

        {error && <div className="error-banner" role="status">{error}</div>}
        {callMuteWarning && <div className="warning-banner" role="status">{callMuteWarning}</div>}

        {showVideoWorkspace ? (
          <ScreenRecordingWorkspace
            status={screenStatus}
            elapsedSeconds={screenElapsedSeconds}
            statusDisplay={<RecorderStatusDisplay className="recording-state" status={screenStatus} elapsedSeconds={screenElapsedSeconds} />}
            targets={screenTargets}
            selectedTarget={selectedScreenTarget}
            codec={videoCodec}
            captureMode={screenAudioCaptureMode}
            audioLevels={screenAudioLevels}
            processingStatus={screenProcessingStatus}
            videos={videos}
            onSelectScreen={(target) => setSelectedScreenTarget(target)}
            onSelectArea={handleSelectArea}
            onCodecChange={setVideoCodec}
            onCaptureModeChange={setScreenAudioCaptureMode}
            onStart={handleStartScreenRecording}
            onStop={handleStopScreenRecording}
            onOpenRecordingsFolder={handleOpenVideoRecordingsFolder}
            onRefreshVideos={handleRefreshVideos}
            onGenerateTranscription={handleGenerateVideoTranscription}
            onDeleteVideo={handleDeleteVideo}
            onRenameVideo={handleRenameVideo}
            refreshingVideos={refreshingVideos}
            transcribing={fastTranscribing}
          />
        ) : showBatchTranscription ? (
          <section className="batch-panel" aria-label="Batch transcription">
            <div>
              <span className="section-kicker">Azure Speech</span>
              <h2>Batch transcription</h2>
              <p>Upload one saved meeting file directly to Azure Speech Fast Transcription.</p>
            </div>
            <div className="batch-notice">
              Batch transcription uses Azure Speech; Foundry Local is available for live recordings.
            </div>
            <p className="batch-privacy">The selected audio is sent directly to Azure Speech. Azure Blob Storage is not used.</p>
            {retranscribeSourceId && <p className="batch-notice">Retranscription creates a new meeting result. The original recording, transcript, language, and speaker names are left untouched.</p>}
            <button type="button" className="batch-file-button" onClick={handleSelectFastTranscriptionFile} disabled={fastTranscribing}>
              <FileAudio size={18} />
              {fastTranscriptionFile ? 'Choose another file' : 'Choose WAV, MP3, or MP4'}
            </button>
            {fastTranscriptionFile && (
              <div className="batch-file-details">
                <strong>{fastTranscriptionFile.name}</strong>
                <span>{formatTimestamp(fastTranscriptionFile.durationSeconds)} · {fastTranscriptionFile.sizeBytes > 0 ? `${(fastTranscriptionFile.sizeBytes / 1024 / 1024).toFixed(1)} MB` : 'File size validated before upload'}</span>
                <span>Estimated completion: about {Math.max(1, Math.ceil(fastTranscriptionFile.durationSeconds / 5 / 60) + 1)} minute(s)</span>
                {fastTranscriptionFile.requiresMp3Extraction && <span>MP4 audio is extracted locally to MP3 before upload. Azure Speech does not manage this conversion.</span>}
              </div>
            )}
            <label>
              Azure Speech language
              <select value={normalizeAzureLanguage(fastLanguage)} onChange={(event) => setFastLanguage(normalizeAzureLanguage(event.target.value))} disabled={fastTranscribing}>
                {AZURE_LANGUAGE_OPTIONS.map((option) => <option key={option.value} value={option.value}>{option.label}</option>)}
              </select>
            </label>
            <label>
              <input type="checkbox" checked={fastDiarization} onChange={(event) => setFastDiarization(event.target.checked)} disabled={fastTranscribing} />
              Identify speakers (optional diarization). Labels are estimates and can be renamed after transcription.
            </label>
            <label>
              <input type="checkbox" checked={fastUploadConsent} onChange={(event) => {
                const checked = event.target.checked && Boolean(fastTranscriptionFile);
                fastConsentRef.current = checked && fastTranscriptionFile ? { file: fastTranscriptionFile, meetingId: activeMeetingId } : null;
                setFastUploadConsent(checked);
              }} disabled={!fastTranscriptionFile || fastTranscribing} />
              I explicitly consent to sending this selected file’s audio to Azure Speech for transcription.
            </label>
            {fastTranscriptionProgress && (
              <div className="batch-progress" role="status">
                <div><span>{fastTranscriptionProgress.phase}</span><strong>{fastTranscriptionProgress.detail}</strong></div>
                <progress max="4" value={{ validating: 1, uploading: 2, transcribing: 3, saving: 4 }[fastTranscriptionProgress.phase]} />
              </div>
            )}
            <button type="button" className="batch-submit" onClick={handleStartFastTranscription} disabled={!fastTranscriptionFile || !fastUploadConsent || fastTranscribing || status !== 'idle' || screenStatus !== 'idle' || recordingOperationBusy}>
              {fastTranscribing ? 'Transcribing...' : 'Start Azure Speech transcription'}
            </button>
          </section>
        ) : <>
        {showSettings && (
          <aside className="settings-panel" aria-label="Settings panel">
            <div className="settings-intro">
              <span className="section-kicker">Meetly Lite</span>
              <h2>Settings</h2>
              <p>Choose the capture source, transcription engine, and language for new recordings.</p>
            </div>
            <div className="settings-tabs" role="tablist" aria-label="Settings categories">
              <button type="button" className={settingsTab === 'audio' ? 'active' : ''} onClick={() => setSettingsTab('audio')}>Audio</button>
              <button type="button" className={settingsTab === 'video' ? 'active' : ''} onClick={() => setSettingsTab('video')}>Video</button>
            </div>
            {settingsTab === 'audio' && <>
            <div className="settings-card primary">
              <div>
                <h3>Session Defaults</h3>
                <p>These settings apply when the next recording starts.</p>
              </div>
              <label>
                Transcription Engine
                <select
                  value={transcriptionEngine}
                  onChange={(event) => setTranscriptionEngine(event.target.value as TranscriptionEngine)}
                  disabled={status !== 'idle'}
                >
                  <option value="foundryLocal">Foundry Local</option>
                  <option value="azure">Azure Speech</option>
                </select>
              </label>
              <label>
                Capture Mode
                <select
                  value={selectedCaptureMode}
                  onChange={(event) => setSelectedCaptureMode(event.target.value)}
                  disabled={status !== 'idle'}
                >
                  {CAPTURE_MODE_OPTIONS.map((option) => (
                    <option key={option.value} value={option.value}>{option.label}</option>
                  ))}
                </select>
              </label>
              <label>
                Audio Input
                <div className="path-picker-row">
                  <select
                    value={selectedAudioInputId}
                    onChange={(event) => setSelectedAudioInputId(event.target.value)}
                    disabled={status !== 'idle' || selectedCaptureMode === 'system'}
                  >
                    {audioInputDevices.map((device) => (
                      <option key={device.id || 'default'} value={device.id}>
                        {device.name}{device.isDefault && device.id ? ' (default)' : ''}
                      </option>
                    ))}
                  </select>
                  <button type="button" onClick={handleRefreshAudioInputs} disabled={status !== 'idle'}>Refresh</button>
                </div>
              </label>
              <label>
                System Audio
                <select
                  value={selectedSystemAudioOutputId}
                  onChange={(event) => setSelectedSystemAudioOutputId(event.target.value)}
                  disabled={status !== 'idle' || selectedCaptureMode === 'microphone'}
                >
                  {audioOutputDevices.map((device) => (
                    <option key={device.id || 'default'} value={device.id}>
                      {device.name}{device.isDefault && device.id ? ' (default)' : ''}
                    </option>
                  ))}
                </select>
              </label>
              <label>
                Session Language
                <select
                  value={transcriptionEngine === 'azure'
                    ? normalizeAzureLanguage(azureLanguage)
                    : normalizeFoundryLanguage(foundryLocalLanguage)}
                  onChange={(event) => {
                    if (transcriptionEngine === 'azure') {
                      setAzureLanguage(event.target.value === 'auto' ? 'en-US' : event.target.value);
                    } else {
                      setFoundryLocalLanguage(event.target.value === 'auto' ? 'en' : event.target.value);
                    }
                  }}
                  disabled={status !== 'idle'}
                >
                  {(transcriptionEngine === 'azure'
                    ? AZURE_LANGUAGE_OPTIONS
                    : FOUNDRY_LANGUAGE_OPTIONS).map((option) => (
                    <option key={option.value} value={option.value}>{option.label}</option>
                  ))}
                </select>
              </label>
            </div>
            {transcriptionEngine === 'azure' && (
              <div className="settings-card primary">
                <div>
                  <h3>Azure Speech</h3>
                  <p>Use Azure Speech with Azure CLI sign-in for real-time recognition.</p>
                </div>
                <label>
                  Speech Custom Domain Endpoint
                  <input
                    value={azureEndpoint}
                    onChange={(event) => setAzureEndpoint(event.target.value)}
                    placeholder="https://your-custom-name.cognitiveservices.azure.com/"
                    disabled={status !== 'idle'}
                  />
                </label>
                <label>
                  Azure CLI Tenant ID
                  <input
                    value={azureTenantId}
                    onChange={(event) => setAzureTenantId(event.target.value)}
                    placeholder="Optional tenant GUID for Azure CLI sign-in"
                    disabled={status !== 'idle'}
                  />
                </label>
                <label>
                  Azure CLI Subscription ID
                  <input
                    value={azureSubscriptionId}
                    onChange={(event) => setAzureSubscriptionId(event.target.value)}
                    placeholder="Optional subscription GUID for Azure CLI account selection"
                    disabled={status !== 'idle'}
                  />
                </label>
                <div className="settings-action-row">
                  <span className={`model-status ${azureCliStatus?.state === 'connected' ? 'loaded' : ''}`}>
                    {azureCliStatus?.state === 'connected' ? 'Signed in' : 'Not signed in'}
                  </span>
                  <button type="button" onClick={handleAzureSignIn} disabled={status !== 'idle' || azureSigningIn}>
                    {azureSigningIn ? 'Signing in...' : 'Sign in'}
                  </button>
                </div>
                {azureCliStatus?.detail && <p>{azureCliStatus.detail}</p>}
              </div>
            )}
            {transcriptionEngine === 'foundryLocal' && (
              <div className="settings-card primary">
                <div>
                  <h3>Foundry Local Speech Model</h3>
                  <p>Use an on-device Foundry Local STT model. Downloading is explicit and cached for offline use.</p>
                </div>
                <label>
                  Model Alias
                  <select
                    value={foundryLocalModelAlias}
                    onChange={(event) => setFoundryLocalModelAlias(event.target.value)}
                    disabled={status !== 'idle' || foundryLoadingModels || Boolean(foundryDownloadingAlias)}
                  >
                    <option value="">Select a speech model</option>
                    {foundryLocalModels.map((model) => (
                      <option key={model.alias} value={model.alias}>
                        {foundryModelLabel(model)}{model.cached ? ' - cached' : ''}
                      </option>
                    ))}
                  </select>
                </label>
                <label>
                  Chunking
                  <select
                    value={foundryLocalChunkingMode}
                    onChange={(event) =>
                      setFoundryLocalChunkingMode(event.target.value as FoundryLocalChunkingMode)
                    }
                    disabled={status !== 'idle'}
                  >
                    <option value="utterance">
                      Utterance-based — better sentence boundaries
                    </option>
                    <option value="fixed5Seconds">
                      Fixed 5 seconds — predictable updates
                    </option>
                  </select>
                </label>
                <div className="settings-action-row">
                  <span className={`model-status ${foundryLocalModels.find((model) => model.alias === foundryLocalModelAlias)?.cached ? 'loaded' : ''}`}>
                    {foundryDownloadingAlias
                      ? `${foundryDownloadProgress?.phase ?? 'model'} ${Math.round(foundryDownloadProgress?.percent ?? 0)}%`
                      : foundryLocalModels.find((model) => model.alias === foundryLocalModelAlias)?.cached
                        ? 'Cached'
                        : foundryLocalModelAlias
                          ? 'Not downloaded'
                          : 'No model selected'}
                  </span>
                  <div className="settings-button-group">
                    <button type="button" onClick={handleRefreshFoundryModels} disabled={status !== 'idle' || foundryLoadingModels || Boolean(foundryDownloadingAlias)}>
                      {foundryLoadingModels ? 'Refreshing...' : 'Refresh'}
                    </button>
                    <button type="button" onClick={handleDownloadFoundryModel} disabled={status !== 'idle' || !foundryLocalModelAlias || Boolean(foundryDownloadingAlias)}>
                      {foundryDownloadingAlias ? 'Downloading...' : 'Download / prepare'}
                    </button>
                  </div>
                </div>
                {foundryDownloadProgress?.message && <p>{foundryDownloadProgress.message}</p>}
              </div>
            )}
            </>}
            {settingsTab === 'video' && (
              <div className="settings-card primary">
                <div>
                  <h3>Screen recording</h3>
                  <p>Video recordings stay local. Select a folder and the codec used for future captures.</p>
                </div>
                <label>
                  Output folder
                  <div className="path-picker-row">
                    <input value={videoOutputFolder} readOnly placeholder="App recordings folder" />
                    <button type="button" onClick={handleChooseVideoOutputFolder} disabled={screenStatus !== 'idle'}>Browse</button>
                  </div>
                </label>
                <label>
                  Video codec
                  <select value={videoCodec} onChange={(event) => setVideoCodec(event.target.value as 'h264' | 'h265')} disabled={screenStatus !== 'idle'}>
                    <option value="h264">H.264</option>
                    <option value="h265">H.265 (HEVC)</option>
                  </select>
                </label>
                <label>
                  FFmpeg executable (optional)
                  <div className="path-picker-row">
                    <input value={ffmpegPath} readOnly placeholder="Bundled FFmpeg runtime" />
                    <button type="button" onClick={handleChooseFfmpegExecutable} disabled={screenStatus !== 'idle'}>Browse</button>
                    {ffmpegPath && <button type="button" onClick={handleUseBundledFfmpeg} disabled={screenStatus !== 'idle'}>Use bundled</button>}
                  </div>
                </label>
                <p>Leave this blank to use the FFmpeg executable packaged with Meetly Lite.</p>
                <p>H.265 needs an FFmpeg build with HEVC support and may be slower to encode.</p>
              </div>
            )}
          </aside>
        )}

        <div className={`meeting-workspace ${assistantExpanded && activeMeeting ? 'with-assistant' : ''}`}>
        <div className="meeting-transcript-column">
        <section className="transcript-surface">
          {!activeMeeting && (
            <div className="empty-state">
              <Volume2 size={24} />
              <h1>Ready to transcribe</h1>
              <p>Start recording, or open Settings to choose a transcription engine.</p>
            </div>
          )}

          {activeMeeting && (
            <>
              <div className="meeting-header">
                <div>
                  <input
                    value={activeMeeting.title}
                    onChange={(event) => {
                      const title = event.target.value;
                      updateActiveMeeting((meeting) => ({ ...meeting, title, updatedAt: new Date().toISOString() }));
                      renameNativeMeeting(activeMeeting.id, title).catch((titleError) => {
                        setError(titleError instanceof Error ? titleError.message : 'Failed to save meeting title.');
                      });
                    }}
                    aria-label="Meeting title"
                  />
                  <span>{new Date(activeMeeting.createdAt).toLocaleString()}</span>
                  <div className="session-meta">
                    <label>
                      <span>Language</span>
                      <select
                        value={activeMeeting.transcriptionEngine?.startsWith('azure')
                          ? normalizeAzureLanguage(activeMeeting.language)
                          : normalizeFoundryLanguage(activeMeeting.language)}
                        onChange={(event) => handleSessionLanguageChange(activeMeeting, event.target.value)}
                        disabled={recordingOperationBusy || status === 'starting' || status === 'saving' || fastTranscribing}
                      >
                        {(activeMeeting.transcriptionEngine === 'azure' || activeMeeting.transcriptionEngine === 'azure-fast' ? AZURE_LANGUAGE_OPTIONS : LANGUAGE_OPTIONS).map((option) => (
                          <option key={option.value} value={option.value}>{option.label}</option>
                        ))}
                      </select>
                    </label>
                    <span>{status === 'recording'
                      ? activeMeeting.transcriptionEngine === 'azure'
                        ? 'Changes apply to upcoming audio; pending audio is buffered during reconnect.'
                        : 'Changes apply at the next utterance boundary.'
                      : 'Saved language is metadata for future recordings. Existing text changes only with a new transcription.'}</span>
                  </div>
                </div>
                <div className="meeting-actions">
                  <button onClick={() => handleTogglePlayback(activeMeeting)} disabled={!activeMeeting.hasAudio} title={playingMeetingId === activeMeeting.id && !playbackPaused ? 'Pause recording playback' : 'Play recording'}>
                    {playingMeetingId === activeMeeting.id && !playbackPaused ? <Pause size={16} /> : <Play size={16} />}
                  </button>
                  <button onClick={() => handleExportTranscript(activeMeeting)} title="Export transcript">
                    <FileText size={16} />
                  </button>
                  <button onClick={() => handleExportAudio(activeMeeting)} disabled={!activeMeeting.hasAudio} title="Export audio">
                    <Download size={16} />
                  </button>
                  <button onClick={() => handleOpenMeetingFolder(activeMeeting)} title="Open local folder">
                    <FolderOpen size={16} />
                  </button>
                  <button type="button" className="text-action" onClick={() => handleRetranscribeMeeting(activeMeeting)} disabled={!activeMeeting.hasAudio || !activeMeeting.recordingPath || status !== 'idle' || screenStatus !== 'idle' || fastTranscribing || recordingOperationBusy} title="Retranscribe saved audio into a new meeting" aria-label="Retranscribe saved audio">
                    <RefreshCw size={16} />
                    Retranscribe
                  </button>
                  <button onClick={() => handleDeleteMeeting(activeMeeting.id)} disabled={status !== 'idle' || recordingOperationBusy || fastTranscribing} title="Delete meeting">
                    <Trash2 size={16} />
                  </button>
                </div>
              </div>

              {meetingSpeakers.length > 0 && (
                <details className="speaker-editor" style={{ width: '100%', maxWidth: 820, maxHeight: 160, overflowY: 'auto', margin: '0 auto 12px' }}>
                  <summary style={{ cursor: 'pointer', padding: '8px 0' }}>Speaker names ({meetingSpeakers.length})</summary>
                  <p>Names apply to every segment with the same speaker label in this meeting.</p>
                  {meetingSpeakers.map((speaker) => (
                    <label key={`${activeMeeting.id}:${speaker.id}:${speaker.name}`} style={{ display: 'inline-flex', alignItems: 'center', gap: 8, margin: '4px 12px 4px 0' }}>
                      <span className="speaker-badge" style={SPEAKER_BADGE_STYLE}>Speaker {speaker.id}</span>
                      <input
                        aria-label={`Name for ${speaker.id}`}
                        defaultValue={speaker.name}
                        placeholder={speaker.id}
                        disabled={status !== 'idle' || fastTranscribing || recordingOperationBusy}
                        onBlur={(event) => {
                          const name = event.currentTarget.value.trim();
                          if (name !== speaker.name) void handleRenameSpeaker(activeMeeting, speaker.id, name);
                        }}
                      />
                    </label>
                  ))}
                </details>
              )}

              <div className="transcript-list" ref={transcriptListRef} aria-live="polite">
                {activeMeeting.transcript.map((segment) => (
                  <article
                    key={segment.id}
                    className={`transcript-row${playingSegmentId === segment.id ? ' is-playing' : ''}`}
                  >
                    <button
                      type="button"
                      className="segment-play"
                      onClick={() => handlePlaySegment(activeMeeting, segment)}
                      disabled={!activeMeeting.hasAudio}
                      title="Play from here"
                      aria-label={`Play audio from ${formatTimestamp(segment.offsetSeconds)}`}
                    >
                      <Play size={12} />
                      <time>{formatTimestamp(segment.offsetSeconds)}</time>
                    </button>
                    <p>{segment.speakerId && <strong className="speaker-badge" style={SPEAKER_BADGE_STYLE}>{segment.speakerName || `Speaker ${segment.speakerId}`}</strong>}{segment.text}</p>
                  </article>
                ))}
                {azurePartialText && (
                  <article className="transcript-row pending">
                    <span className="segment-play">live</span>
                    <p>{azurePartialText}</p>
                  </article>
                )}
                {activeMeeting.transcript.length === 0 && !azurePartialText && (
                  <div className="empty-state small">
                    <p>Recording is ready. Transcript text will stream in as it is produced.</p>
                  </div>
                )}
              </div>
            </>
          )}
        </section>

        <div className="floating-recorder">
          <RecordButton
            className="primary-stop"
            status={status}
            iconSize={20}
            onStart={handleOpenRecordingChoice}
            onStop={handleStopRecording}
          />
        </div>
        </div>
        {activeMeeting && assistantExpanded && <aside id="meeting-assistant-panel" className="meeting-assistant-dock" aria-label="Meeting Copilot">
          <MeetingAssistant
            meeting={activeMeeting}
            recording={status === 'recording' && recordingMeetingIdRef.current === activeMeeting.id}
            onClose={() => setAssistantExpanded(false)}
            onSeek={(seconds) => { void handleAssistantSeek(activeMeeting, seconds); }}
            playbackDisabled={status !== 'idle' || !activeMeeting.hasAudio}
            disabled={status === 'starting' || status === 'saving' || recordingOperationBusy}
          />
        </aside>}
        </div>
        </>}
      </main>

      {pendingMeetingId && (
        <div className="modal-backdrop" role="presentation" onClick={handleCancelSwitch}>
          <div
            className="modal-card"
            role="alertdialog"
            aria-modal="true"
            aria-labelledby="switch-warning-title"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 id="switch-warning-title">Stop current recording?</h2>
            <p>A recording is still in progress. Switching to another session will stop and save the current recording.</p>
            <div className="modal-actions">
              <button type="button" className="modal-secondary" onClick={handleCancelSwitch}>
                Keep recording
              </button>
              <button type="button" className="modal-danger" onClick={handleConfirmSwitch}>
                Stop &amp; switch
              </button>
            </div>
          </div>
        </div>
      )}
      {showRecordingChoice && (
        <div className="modal-backdrop" role="presentation" onClick={() => setShowRecordingChoice(false)}>
          <div
            className="modal-card"
            role="dialog"
            aria-modal="true"
            aria-labelledby="recording-choice-title"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 id="recording-choice-title">Start recording</h2>
            <p>Choose whether to create a new meeting session or add this recording to the session currently open.</p>
            {activeMeeting && <p>Append uses this meeting’s language ({activeMeeting.language}) and {activeMeeting.transcriptionEngine?.startsWith('azure') ? 'Azure Speech live recognition (audio is sent to Azure)' : 'Foundry Local recognition'}, without changing your defaults.</p>}
            <div className="modal-actions recording-choice-actions">
              <button type="button" className="modal-secondary" onClick={() => setShowRecordingChoice(false)}>
                Cancel
              </button>
              <button type="button" className="modal-secondary" onClick={handleStartNewRecording} disabled={status !== 'idle' || recordingOperationBusy || fastTranscribing}>
                New recording
              </button>
              <button
                type="button"
                className="modal-danger"
                onClick={handleAppendToOpenMeeting}
                disabled={!activeMeeting || status !== 'idle' || recordingOperationBusy || fastTranscribing}
                title={activeMeeting ? `Append to ${activeMeeting.title}` : 'Open a saved meeting to append to it'}
              >
                Append to open session
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
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
import type { AppSettings, FastTranscriptionFile, FastTranscriptionProgress, Meeting, RecorderStatus, ScreenAudioLevels, ScreenProcessingStatus, ScreenTarget, TranscriptSegment, TranscriptionEngine, VideoRecording } from './types';
import {
  addNativeTranscriptSegment,
  checkAzureCliSignIn,
  fetchMeetings,
  refreshMeetings,
  deleteNativeMeeting,
  deleteNativeVideo,
  renameNativeVideo,
  exportNativeAudio,
  exportNativeTranscript,
  getNativeSettings,
  getRecordingRuntimeStatus,
  getWhisperModelStatus,
  listNativeAudioInputDevices,
  listNativeAudioOutputDevices,
  loadWhisperModel,
  onAudioChunk,
  onTranscriptSegment,
  openNativeMeetingFolder,
  onFastTranscriptionProgress,
  pauseNativePlayback,
  playNativeRecording,
  renameNativeMeeting,
  resumeNativePlayback,
  selectWhisperModel,
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
import type { AzureCliStatus } from './nativeClient';
import { formatTimestamp } from './exporters';
import { AZURE_LANGUAGE_OPTIONS, CAPTURE_MODE_OPTIONS, LANGUAGE_OPTIONS } from './audio/recordingOptions';
import { ScreenRecordingWorkspace } from './video/ScreenRecordingWorkspace';

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

type RecorderStatusDisplayProps = {
  className: string;
  status: RecorderStatus;
  elapsedSeconds: number;
};

function RecorderStatusDisplay({ className, status, elapsedSeconds }: RecorderStatusDisplayProps) {
  return (
    <div className={className}>
      <span className={`state-dot ${status}`} />
      <div>
        <strong>{getRecorderStatusLabel(status)}</strong>
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
  const [whisperModelLoaded, setWhisperModelLoaded] = useState(false);
  const [whisperModelPath, setWhisperModelPath] = useState('');
  const [audioInputDevices, setAudioInputDevices] = useState([{ id: '', name: 'System default', isDefault: true }]);
  const [audioOutputDevices, setAudioOutputDevices] = useState([{ id: '', name: 'System default', isDefault: true }]);
  const [selectedAudioInputId, setSelectedAudioInputId] = useState('');
  const [selectedSystemAudioOutputId, setSelectedSystemAudioOutputId] = useState('');
  const [selectedCaptureMode, setSelectedCaptureMode] = useState('microphoneSystem');
  const [selectedLanguage, setSelectedLanguage] = useState('en');
  const [transcriptionEngine, setTranscriptionEngine] = useState<TranscriptionEngine>('local');
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

  const elapsedTimerRef = useRef<number | null>(null);
  const screenElapsedTimerRef = useRef<number | null>(null);
  const activeScreenVideoIdRef = useRef<string | null>(null);
  const activeMeetingIdRef = useRef<string | null>(null);
  const elapsedSecondsRef = useRef(0);
  const transcriptListRef = useRef<HTMLDivElement | null>(null);
  const azureSessionRef = useRef<AzureSpeechSession | null>(null);
  const audioChunkCleanupRef = useRef<(() => void) | null>(null);
  const settingsLoadedRef = useRef(false);

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
    videoOutputFolder,
    videoCodec,
    ffmpegPath,
    ...overrides,
  });

  const activeMeeting = useMemo(
    () => meetings.find((meeting) => meeting.id === activeMeetingId) ?? null,
    [activeMeetingId, meetings],
  );

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
    const load = async () => {
      try {
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
          if (storedSettings.transcriptionEngine === 'local' || storedSettings.transcriptionEngine === 'azure') {
            setTranscriptionEngine(storedSettings.transcriptionEngine);
          }
          if (storedSettings.captureMode) setSelectedCaptureMode(storedSettings.captureMode);
          if (storedSettings.screenAudioCaptureMode) {
            setScreenAudioCaptureMode(storedSettings.screenAudioCaptureMode);
          } else if (storedSettings.captureMode) {
            setScreenAudioCaptureMode(storedSettings.captureMode);
          }
          if (storedSettings.audioDeviceId) setSelectedAudioInputId(storedSettings.audioDeviceId);
          if (storedSettings.systemAudioDeviceId) setSelectedSystemAudioOutputId(storedSettings.systemAudioDeviceId);
          if (storedSettings.language) setSelectedLanguage(storedSettings.language);
          if (storedSettings.azureEndpoint) setAzureEndpoint(storedSettings.azureEndpoint);
          if (storedSettings.azureTenantId) setAzureTenantId(storedSettings.azureTenantId);
          if (storedSettings.azureSubscriptionId) setAzureSubscriptionId(storedSettings.azureSubscriptionId);
          if (storedSettings.azureLanguage) setAzureLanguage(storedSettings.azureLanguage);
          if (storedSettings.videoOutputFolder) setVideoOutputFolder(storedSettings.videoOutputFolder);
          if (storedSettings.videoCodec === 'h265' || storedSettings.videoCodec === 'h264') setVideoCodec(storedSettings.videoCodec);
          if (storedSettings.ffmpegPath) setFfmpegPath(storedSettings.ffmpegPath);
        }
        void checkAzureCliSignIn(
          storedSettings?.azureTenantId?.trim() || undefined,
          storedSettings?.azureSubscriptionId?.trim() || undefined,
        )
          .then((status) => setAzureCliStatus(status))
          .catch(() => undefined);
        const modelStatus = await getWhisperModelStatus();
        setWhisperModelLoaded(modelStatus.loaded);
        if (modelStatus.path) {
          setWhisperModelPath(modelStatus.path);
          if (!modelStatus.loaded) {
            void ensureWhisperModelLoaded(modelStatus.path, false);
          }
        }
        const devices = await listNativeAudioInputDevices();
        setAudioInputDevices(devices.length > 0 ? devices : [{ id: '', name: 'System default', isDefault: true }]);
        const outputDevices = await listNativeAudioOutputDevices();
        setAudioOutputDevices(outputDevices.length > 0 ? outputDevices : [{ id: '', name: 'System default', isDefault: true }]);
        const runtimeStatus = await getRecordingRuntimeStatus();
        if (runtimeStatus.audioRecordingActive && runtimeStatus.audioMeetingId) {
          const restoredElapsed = Math.floor(runtimeStatus.audioElapsedSeconds);
          activeMeetingIdRef.current = runtimeStatus.audioMeetingId;
          setActiveMeetingId(runtimeStatus.audioMeetingId);
          setElapsedSeconds(restoredElapsed);
          elapsedSecondsRef.current = restoredElapsed;
          setStatus('recording');
          if (!elapsedTimerRef.current) startTimers();

          const activeRecording = storedMeetings.find((meeting) => meeting.id === runtimeStatus.audioMeetingId);
          if (activeRecording?.transcriptionEngine === 'azure' && storedSettings?.azureEndpoint?.trim()) {
            try {
              const azureSession = await startAzureSpeechSession({
                endpoint: storedSettings.azureEndpoint.trim(),
                tenantId: storedSettings.azureTenantId?.trim() || undefined,
                subscriptionId: storedSettings.azureSubscriptionId?.trim() || undefined,
                language: activeRecording.language || storedSettings.azureLanguage?.trim() || 'en-US',
                onPartialText: setAzurePartialText,
                onFinalText: async (text, offsetSeconds) => {
                  const meetingId = activeMeetingIdRef.current;
                  if (meetingId) await addNativeTranscriptSegment(meetingId, text, offsetSeconds);
                },
                onError: setError,
              });
              azureSessionRef.current = azureSession;
              audioChunkCleanupRef.current = await onAudioChunk((event) => {
                if (event.meetingId === activeMeetingIdRef.current) {
                  azureSessionRef.current?.pushPcmBase64(event.pcmBase64);
                }
              });
            } catch (azureError) {
              setError(getErrorMessage(azureError, 'Recording restored, but Azure live transcription could not reconnect.'));
            }
          }
        }
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
      } catch (loadError) {
        setError(loadError instanceof Error ? loadError.message : 'Tauri backend is not available.');
      } finally {
        settingsLoadedRef.current = true;
      }
    };

    load();

    let unlisten: (() => void) | undefined;
    onTranscriptSegment(({ meetingId, segment }) => {
      setMeetings((current) => {
        const target = current.find((meeting) => meeting.id === meetingId);
        if (!target) {
          return current;
        }
        return upsertMeeting(current, {
          ...target,
          transcript: [...target.transcript, segment],
          updatedAt: new Date().toISOString(),
        });
      });
    }).then((cleanup) => {
      unlisten = cleanup;
    });

    return () => {
      unlisten?.();
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
      audioChunkCleanupRef.current?.();
      void azureSessionRef.current?.stop();
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

  const ensureWhisperModelLoaded = async (modelPath: string, showErrors: boolean) => {
    const trimmedPath = modelPath.trim();
    if (!trimmedPath) {
      if (showErrors) {
        setError('Enter the full path to a local Whisper .bin model file.');
      }
      return false;
    }

    if (whisperModelLoaded && whisperModelPath === trimmedPath) {
      return true;
    }

    try {
      await loadWhisperModel(trimmedPath);
      setWhisperModelPath(trimmedPath);
      setWhisperModelLoaded(true);
      return true;
    } catch (modelError) {
      setWhisperModelLoaded(false);
      if (showErrors) {
        setError(modelError instanceof Error ? modelError.message : String(modelError));
      }
      return false;
    }
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
      updateActiveMeeting((meeting) => ({
        ...meeting,
        durationSeconds: meeting.durationSeconds + 1,
        updatedAt: new Date().toISOString(),
      }));
    }, 1000);
  };

  const stopTimers = () => {
    if (elapsedTimerRef.current) {
      window.clearInterval(elapsedTimerRef.current);
      elapsedTimerRef.current = null;
    }
  };

  const handleStartRecording = async (appendToMeetingId?: string) => {
    setError(null);
    setStatus('starting');
    setShowRecordingList(false);
    setAzurePartialText('');
    const appendOffsetSeconds = appendToMeetingId === activeMeeting?.id
      ? activeMeeting?.durationSeconds ?? 0
      : 0;
    let azureSession: AzureSpeechSession | null = null;
    let audioChunkCleanup: (() => void) | null = null;
    try {
      // Trigger the same load path as the "Load Model" button so the Whisper
      // model is ready without a manual step. If no path is configured yet, ask
      // the backend for an auto-detected model. Loading here is best-effort: the
      // backend also auto-loads/detects a model when recording starts, so we do
      // not block on a frontend load failure.
      if (transcriptionEngine === 'local') {
        let modelPath = whisperModelPath.trim();
        if (!modelPath) {
          const status = await getWhisperModelStatus();
          if (status.path) {
            modelPath = status.path;
            setWhisperModelPath(status.path);
          }
        }
        if (modelPath) {
          await ensureWhisperModelLoaded(modelPath, false);
        }
      } else {
        const resolvedAzureEndpoint = azureEndpoint.trim();
        if (!resolvedAzureEndpoint) {
          throw new Error('Enter the Speech custom domain endpoint before recording with Azure.');
        }
        const signedIn = await ensureAzureCliSignedIn();
        if (!signedIn) {
          throw new Error('Azure CLI sign-in is required before recording with Azure Speech.');
        }
        azureSession = await startAzureSpeechSession({
          endpoint: resolvedAzureEndpoint,
          tenantId: tenantForAzureCli(),
          subscriptionId: subscriptionForAzureCli(),
          language: azureLanguage.trim() || 'en-US',
          onPartialText: setAzurePartialText,
          onFinalText: async (text, offsetSeconds) => {
            const meetingId = activeMeetingIdRef.current;
            if (meetingId) {
              await addNativeTranscriptSegment(meetingId, text, offsetSeconds + appendOffsetSeconds);
              setAzurePartialText('');
            }
          },
          onError: setError,
        });
        audioChunkCleanup = await onAudioChunk((event) => {
          if (event.meetingId === activeMeetingIdRef.current) {
            azureSessionRef.current?.pushPcmBase64(event.pcmBase64);
          }
        });
      }
      const meeting = await startNativeRecording({
        appendToMeetingId,
        audioDeviceId: selectedAudioInputId,
        systemAudioDeviceId: selectedSystemAudioOutputId,
        captureMode: selectedCaptureMode,
        language: transcriptionEngine === 'azure' ? azureLanguage.trim() || 'en-US' : selectedLanguage,
        transcriptionEngine,
      });
      azureSessionRef.current = azureSession;
      audioChunkCleanupRef.current = audioChunkCleanup;
      setMeetings((current) => upsertMeeting(current, meeting));
      setActiveMeetingId(meeting.id);
      activeMeetingIdRef.current = meeting.id;
      setElapsedSeconds(appendOffsetSeconds);
      elapsedSecondsRef.current = appendOffsetSeconds;
      setStatus('recording');
      startTimers();
      // Reflect the model the backend actually loaded (e.g. an auto-detected one).
      if (transcriptionEngine === 'local') {
        getWhisperModelStatus()
          .then((status) => {
            setWhisperModelLoaded(status.loaded);
            if (status.path) {
              setWhisperModelPath(status.path);
            }
          })
          .catch(() => undefined);
      }
    } catch (recordingError) {
      audioChunkCleanup?.();
      await azureSession?.stop();
      setStatus('idle');
      setError(getErrorMessage(recordingError, 'Native recording could not start.'));
    }
  };

  const handleOpenRecordingChoice = () => {
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
    setError(null);
    try {
      const file = await selectFastTranscriptionAudio();
      if (file) {
        setFastTranscriptionFile(file);
        setFastTranscriptionProgress(null);
      }
    } catch (selectionError) {
      setError(getErrorMessage(selectionError, 'Could not select an audio file.'));
    }
  };

  const transcribeFile = async (file: FastTranscriptionFile): Promise<boolean> => {
    if (fastTranscribing) {
      return false;
    }
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
      const meeting = await transcribeFastAudio(
        file.path,
        file.name.replace(/\.[^.]+$/, ''),
        azureLanguage.trim() || 'en-US',
        azureEndpoint,
        tenantForAzureCli(),
        subscriptionForAzureCli(),
      );
      setMeetings((current) => upsertMeeting(current, meeting));
      setActiveMeetingId(meeting.id);
      setFastTranscriptionProgress({ phase: 'saving', detail: 'Transcription saved locally.' });
      return true;
    } catch (transcriptionError) {
      setError(getErrorMessage(transcriptionError, 'Azure Speech file transcription failed.'));
      return false;
    } finally {
      setFastTranscribing(false);
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
    }
  };

  const handleStopRecording = async () => {
    setStatus('saving');
    stopTimers();
    try {
      const meeting = await stopNativeRecording();
      audioChunkCleanupRef.current?.();
      audioChunkCleanupRef.current = null;
      await azureSessionRef.current?.stop();
      azureSessionRef.current = null;
      setAzurePartialText('');
      setMeetings((current) => upsertMeeting(current, meeting));
      setStatus('idle');
    } catch (stopError) {
      setError(stopError instanceof Error ? stopError.message : 'Failed to stop recording.');
      audioChunkCleanupRef.current?.();
      audioChunkCleanupRef.current = null;
      await azureSessionRef.current?.stop();
      azureSessionRef.current = null;
      setAzurePartialText('');
      setStatus('idle');
      void refreshMeetings().then(setMeetings).catch(() => undefined);
    }
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
    const completed = await transcribeFile({
      path: video.videoPath,
      name: video.title,
      sizeBytes: 0,
      durationSeconds: video.durationSeconds,
      requiresMp3Extraction: true,
    });
    if (completed) {
      setShowVideoWorkspace(false);
      setShowRecordingList(false);
    }
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
    if (status === 'recording' || status === 'saving') {
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
    setActiveMeetingId(targetId);
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

  const handleLoadWhisperModel = async () => {
    setError(null);
    await ensureWhisperModelLoaded(whisperModelPath, true);
  };

  const handleBrowseWhisperModel = async () => {
    setError(null);
    try {
      const selectedPath = await selectWhisperModel();
      if (selectedPath) {
        setWhisperModelPath(selectedPath);
      }
    } catch (dialogError) {
      setError(dialogError instanceof Error ? dialogError.message : String(dialogError));
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

  const handleSessionLanguageChange = async (meeting: Meeting, language: string) => {
    if (meeting.transcriptionEngine === 'azure-fast') {
      return;
    }
    updateActiveMeeting((currentMeeting) => ({ ...currentMeeting, language, updatedAt: new Date().toISOString() }));
    try {
      if (meeting.transcriptionEngine === 'azure' && status === 'recording') {
        await azureSessionRef.current?.stop();
        azureSessionRef.current = await startAzureSpeechSession({
          endpoint: azureEndpoint,
          tenantId: tenantForAzureCli(),
          subscriptionId: subscriptionForAzureCli(),
          language,
          onPartialText: setAzurePartialText,
          onFinalText: async (text, offsetSeconds) => {
            const meetingId = activeMeetingIdRef.current;
            if (meetingId) await addNativeTranscriptSegment(meetingId, text, offsetSeconds);
          },
          onError: setError,
        });
        setAzureLanguage(language);
      }
      const updatedMeeting = await updateNativeMeetingLanguage(meeting.id, language);
      setMeetings((current) => upsertMeeting(current, updatedMeeting));
      setError(null);
    } catch (languageError) {
      setError(languageError instanceof Error ? languageError.message : 'Failed to change the transcription language.');
    }
  };

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
            <RecorderStatusDisplay className="mini-status-group" status={status} elapsedSeconds={elapsedSeconds} />
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
                  <p>{segment.text}</p>
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
    <div className={`app-shell ${showRecordingList ? 'list-open' : ''}`}>
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
          <RecorderStatusDisplay className="recording-state" status={status} elapsedSeconds={elapsedSeconds} />
          <div className="top-actions">
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
              Batch transcription supports Azure Speech only. Local Whisper is unavailable in this mode.
            </div>
            <p className="batch-privacy">The selected audio is sent directly to Azure Speech. Azure Blob Storage is not used.</p>
            <button type="button" className="batch-file-button" onClick={handleSelectFastTranscriptionFile} disabled={fastTranscribing}>
              <FileAudio size={18} />
              {fastTranscriptionFile ? 'Choose another file' : 'Choose WAV, MP3, or MP4'}
            </button>
            {fastTranscriptionFile && (
              <div className="batch-file-details">
                <strong>{fastTranscriptionFile.name}</strong>
                <span>{formatTimestamp(fastTranscriptionFile.durationSeconds)} · {(fastTranscriptionFile.sizeBytes / 1024 / 1024).toFixed(1)} MB</span>
                <span>Estimated completion: about {Math.max(1, Math.ceil(fastTranscriptionFile.durationSeconds / 5 / 60) + 1)} minute(s)</span>
                {fastTranscriptionFile.requiresMp3Extraction && <span>MP4 audio is extracted locally to MP3 before upload. Azure Speech does not manage this conversion.</span>}
              </div>
            )}
            <label>
              Azure Speech language
              <select value={azureLanguage} onChange={(event) => setAzureLanguage(event.target.value)} disabled={fastTranscribing}>
                {AZURE_LANGUAGE_OPTIONS.map((option) => <option key={option.value} value={option.value}>{option.label}</option>)}
              </select>
            </label>
            {fastTranscriptionProgress && (
              <div className="batch-progress" role="status">
                <div><span>{fastTranscriptionProgress.phase}</span><strong>{fastTranscriptionProgress.detail}</strong></div>
                <progress max="4" value={{ validating: 1, uploading: 2, transcribing: 3, saving: 4 }[fastTranscriptionProgress.phase]} />
              </div>
            )}
            <button type="button" className="batch-submit" onClick={handleStartFastTranscription} disabled={!fastTranscriptionFile || fastTranscribing}>
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
                  <option value="local">Local Whisper</option>
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
                  value={transcriptionEngine === 'azure' ? azureLanguage : selectedLanguage}
                  onChange={(event) => {
                    if (transcriptionEngine === 'azure') {
                      setAzureLanguage(event.target.value === 'auto' ? 'en-US' : event.target.value);
                    } else {
                      setSelectedLanguage(event.target.value);
                    }
                  }}
                  disabled={status !== 'idle'}
                >
                  {(transcriptionEngine === 'azure' ? AZURE_LANGUAGE_OPTIONS : LANGUAGE_OPTIONS).map((option) => (
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
            {transcriptionEngine === 'local' && (
              <div className="settings-card primary">
                <div>
                  <h3>Transcription Model</h3>
                  <p>Use a local ggml Whisper model. If the path is blank, Meetly tries known model folders when recording starts.</p>
                </div>
                <label>
                  Whisper Model Path
                  <div className="path-picker-row">
                    <input
                      value={whisperModelPath}
                      onChange={(event) => setWhisperModelPath(event.target.value)}
                      placeholder="C:\\models\\ggml-base.en.bin"
                    />
                    <button type="button" onClick={handleBrowseWhisperModel}>Browse</button>
                  </div>
                </label>
                <div className="settings-action-row">
                  <span className={`model-status ${whisperModelLoaded ? 'loaded' : ''}`}>
                    {whisperModelLoaded ? 'Loaded' : whisperModelPath ? 'Path selected' : 'Auto-detect'}
                  </span>
                  <button type="button" onClick={handleLoadWhisperModel}>Load selected model</button>
                </div>
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
                        value={activeMeeting.language ?? 'en'}
                        onChange={(event) => handleSessionLanguageChange(activeMeeting, event.target.value)}
                        disabled={activeMeeting.transcriptionEngine === 'azure-fast'}
                      >
                        {(activeMeeting.transcriptionEngine === 'azure' || activeMeeting.transcriptionEngine === 'azure-fast' ? AZURE_LANGUAGE_OPTIONS : LANGUAGE_OPTIONS).map((option) => (
                          <option key={option.value} value={option.value}>{option.label}</option>
                        ))}
                      </select>
                    </label>
                    {activeMeeting.transcriptionEngine === 'azure-fast' && <span>Language is fixed after file submission.</span>}
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
                  <button onClick={() => handleDeleteMeeting(activeMeeting.id)} title="Delete meeting">
                    <Trash2 size={16} />
                  </button>
                </div>
              </div>

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
                    <p>{segment.text}</p>
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
            <div className="modal-actions recording-choice-actions">
              <button type="button" className="modal-secondary" onClick={() => setShowRecordingChoice(false)}>
                Cancel
              </button>
              <button type="button" className="modal-secondary" onClick={handleStartNewRecording}>
                New recording
              </button>
              <button
                type="button"
                className="modal-danger"
                onClick={handleAppendToOpenMeeting}
                disabled={!activeMeeting}
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
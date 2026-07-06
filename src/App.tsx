import { useEffect, useMemo, useRef, useState } from 'react';
import {
  CalendarDays,
  CircleStop,
  Download,
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
  Volume2,
} from 'lucide-react';
import type { Meeting, RecorderStatus, TranscriptSegment } from './types';
import {
  fetchMeetings,
  deleteNativeMeeting,
  exportNativeAudio,
  exportNativeTranscript,
  getWhisperModelStatus,
  listNativeAudioInputDevices,
  listNativeAudioOutputDevices,
  loadWhisperModel,
  onTranscriptSegment,
  openNativeMeetingFolder,
  pauseNativePlayback,
  playNativeRecording,
  renameNativeMeeting,
  resumeNativePlayback,
  selectWhisperModel,
  startNativeRecording,
  stopNativeRecording,
  setNativeMiniMode,
  updateNativeMeetingLanguage,
} from './nativeClient';
import { formatTimestamp } from './exporters';

const LANGUAGE_OPTIONS = [
  { value: 'auto', label: 'Auto detect' },
  { value: 'en', label: 'English' },
  { value: 'ko', label: 'Korean' },
  { value: 'ja', label: 'Japanese' },
  { value: 'zh', label: 'Chinese' },
  { value: 'es', label: 'Spanish' },
  { value: 'fr', label: 'French' },
  { value: 'de', label: 'German' },
];

const STOP_RECORDING_TIMEOUT_MS = 10_000;

const CAPTURE_MODE_OPTIONS = [
  { value: 'microphoneSystem', label: 'Microphone + system' },
  { value: 'microphone', label: 'Microphone only' },
  { value: 'system', label: 'System audio only' },
];

function getLanguageLabel(language: string): string {
  return LANGUAGE_OPTIONS.find((option) => option.value === language)?.label ?? language.toUpperCase();
}

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
  return (
    <button
      className={className}
      title={title}
      onClick={isIdle ? onStart : onStop}
      disabled={status === 'saving'}
    >
      {isIdle ? <Mic size={iconSize} /> : <CircleStop size={iconSize} />}
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
  const [pendingMeetingId, setPendingMeetingId] = useState<string | null>(null);
  const [miniMode, setMiniMode] = useState(false);
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

  const elapsedTimerRef = useRef<number | null>(null);
  const activeMeetingIdRef = useRef<string | null>(null);
  const elapsedSecondsRef = useRef(0);
  const transcriptListRef = useRef<HTMLDivElement | null>(null);

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
      } catch (loadError) {
        setError(loadError instanceof Error ? loadError.message : 'Tauri backend is not available.');
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
    activeMeetingIdRef.current = activeMeetingId;
  }, [activeMeetingId]);

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

  const handleStartRecording = async () => {
    setError(null);
    setShowRecordingList(false);
    try {
      // Trigger the same load path as the "Load Model" button so the Whisper
      // model is ready without a manual step. If no path is configured yet, ask
      // the backend for an auto-detected model. Loading here is best-effort: the
      // backend also auto-loads/detects a model when recording starts, so we do
      // not block on a frontend load failure.
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
      const meeting = await startNativeRecording({
        audioDeviceId: selectedAudioInputId,
        systemAudioDeviceId: selectedSystemAudioOutputId,
        captureMode: selectedCaptureMode,
        language: selectedLanguage,
      });
      setMeetings((current) => upsertMeeting(current, meeting));
      setActiveMeetingId(meeting.id);
      activeMeetingIdRef.current = meeting.id;
      setElapsedSeconds(0);
      elapsedSecondsRef.current = 0;
      setStatus('recording');
      startTimers();
      // Reflect the model the backend actually loaded (e.g. an auto-detected one).
      getWhisperModelStatus()
        .then((status) => {
          setWhisperModelLoaded(status.loaded);
          if (status.path) {
            setWhisperModelPath(status.path);
          }
        })
        .catch(() => undefined);
    } catch (recordingError) {
      setError(getErrorMessage(recordingError, 'Native recording could not start.'));
    }
  };

  const handleStopRecording = async () => {
    setStatus('saving');
    stopTimers();
    try {
      const meeting = await Promise.race([
        stopNativeRecording(),
        new Promise<never>((_, reject) => {
          window.setTimeout(() => reject(new Error('Timed out while stopping recording.')), STOP_RECORDING_TIMEOUT_MS);
        }),
      ]);
      setMeetings((current) => upsertMeeting(current, meeting));
      setStatus('idle');
    } catch (stopError) {
      setError(stopError instanceof Error ? stopError.message : 'Failed to stop recording.');
      setStatus('recording');
      startTimers();
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

  const handleSessionLanguageChange = (meeting: Meeting, language: string) => {
    updateActiveMeeting((currentMeeting) => ({ ...currentMeeting, language, updatedAt: new Date().toISOString() }));
    updateNativeMeetingLanguage(meeting.id, language)
      .then((updatedMeeting) => {
        setMeetings((current) => upsertMeeting(current, updatedMeeting));
        setError(null);
      })
      .catch((languageError) => {
        setError(languageError instanceof Error ? languageError.message : 'Failed to save session language.');
      });
  };

  const handleMiniModeChange = (enabled: boolean) => {
    setMiniMode(enabled);
    setNativeMiniMode(enabled).catch((modeError) => {
      setError(modeError instanceof Error ? modeError.message : 'Failed to resize the app window.');
    });
  };

  if (miniMode) {
    return (
      <div className="mini-shell">
        <header className="mini-topbar">
          <RecorderStatusDisplay className="mini-status-group" status={status} elapsedSeconds={elapsedSeconds} />
          <div className="mini-actions">
            <RecordButton
              className="mini-record"
              status={status}
              iconSize={17}
              onStart={handleStartRecording}
              onStop={handleStopRecording}
              title={status === 'idle' ? 'Start recording' : 'Stop recording'}
            />
            <button onClick={() => handleMiniModeChange(false)} title="Exit mini mode">
              <Maximize2 size={16} />
            </button>
          </div>
        </header>

        <main className="mini-transcript" ref={transcriptListRef} aria-live="polite">
          {activeMeeting?.transcript.length ? (
            activeMeeting.transcript.map((segment) => (
              <div className="mini-transcript-row" key={segment.id}>
                <span>{formatTimestamp(segment.offsetSeconds)}</span>
                <p>{segment.text}</p>
              </div>
            ))
          ) : (
            <div className="mini-transcript-empty">
              <MessageSquareText size={15} />
              <p>Live transcript will appear here.</p>
            </div>
          )}
        </main>
      </div>
    );
  }

  return (
    <div className={`app-shell ${showRecordingList ? 'list-open' : ''}`}>
      <aside className="rail" aria-label="Primary navigation">
        <RecordButton
          className={`rail-button ${status === 'recording' ? 'recording' : ''}`}
          status={status}
          iconSize={20}
          onStart={handleStartRecording}
          onStop={handleStopRecording}
          title={status === 'idle' ? 'Start recording' : 'Stop recording'}
        />
        <button
          className={`rail-button ${showRecordingList ? 'active' : ''}`}
          title="Toggle recordings"
          onClick={() => setShowRecordingList((value) => !value)}
        >
          <CalendarDays size={20} />
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

        <div className="meeting-section-label">Recordings</div>
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
            <button onClick={() => setShowSettings((value) => !value)} title="Settings" aria-label="Settings">
              <SlidersHorizontal size={16} />
            </button>
          </div>
        </header>

        {error && <div className="error-banner" role="status">{error}</div>}

        {showSettings && (
          <aside className="settings-panel" aria-label="Settings panel">
            <div className="settings-intro">
              <span className="section-kicker">Meetly Lite</span>
              <h2>Settings</h2>
              <p>Local transcription runs in the Rust backend. Configure the model before recording.</p>
            </div>
            <div className="settings-card primary">
              <div>
                <h3>Session Defaults</h3>
                <p>Choose the input and language used when the next recording starts.</p>
              </div>
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
                  value={selectedLanguage}
                  onChange={(event) => setSelectedLanguage(event.target.value)}
                  disabled={status !== 'idle'}
                >
                  {LANGUAGE_OPTIONS.map((option) => (
                    <option key={option.value} value={option.value}>{option.label}</option>
                  ))}
                </select>
              </label>
            </div>
            <div className="settings-card primary">
              <div>
                <h3>Transcription Model</h3>
                <p>Connect a local speech-to-text model used by the Rust backend.</p>
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
                  {whisperModelLoaded ? 'loaded' : whisperModelPath ? 'not loaded' : 'missing'}
                </span>
                <button type="button" onClick={handleLoadWhisperModel}>Load Model</button>
              </div>
            </div>
          </aside>
        )}

        <section className="transcript-surface">
          {!activeMeeting && (
            <div className="empty-state">
              <Volume2 size={24} />
              <h1>Ready for local transcription</h1>
              <p>Load a Whisper model, then start recording.</p>
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
                      >
                        {LANGUAGE_OPTIONS.map((option) => (
                          <option key={option.value} value={option.value}>{option.label}</option>
                        ))}
                      </select>
                    </label>
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
                {activeMeeting.transcript.length === 0 && (
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
            onStart={handleStartRecording}
            onStop={handleStopRecording}
          />
        </div>
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
    </div>
  );
}
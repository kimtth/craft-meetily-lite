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
} from './nativeClient';
import { formatTimestamp } from './exporters';

function upsertMeeting(meetings: Meeting[], meeting: Meeting): Meeting[] {
  const existing = meetings.findIndex((item) => item.id === meeting.id);
  if (existing === -1) {
    return [meeting, ...meetings];
  }
  const next = [...meetings];
  next[existing] = meeting;
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
  const [miniMode, setMiniMode] = useState(false);
  const [playingMeetingId, setPlayingMeetingId] = useState<string | null>(null);
  const [playingSegmentId, setPlayingSegmentId] = useState<string | null>(null);
  const [playbackPaused, setPlaybackPaused] = useState(false);
  const [whisperModelStatus, setWhisperModelStatus] = useState('missing');
  const [whisperModelPath, setWhisperModelPath] = useState('');

  const elapsedTimerRef = useRef<number | null>(null);
  const activeMeetingIdRef = useRef<string | null>(null);
  const elapsedSecondsRef = useRef(0);

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

  const latestLine = visibleTranscript[visibleTranscript.length - 1]?.text ?? 'Live transcript will appear here.';

  useEffect(() => {
    const load = async () => {
      try {
        const storedMeetings = await fetchMeetings();
        setMeetings(storedMeetings);
        const modelStatus = await getWhisperModelStatus();
        setWhisperModelStatus(modelStatus);
        if (modelStatus !== 'loaded' && modelStatus !== 'missing') {
          setWhisperModelPath(modelStatus);
        }
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
    try {
      const meeting = await startNativeRecording();
      setMeetings((current) => upsertMeeting(current, meeting));
      setActiveMeetingId(meeting.id);
      activeMeetingIdRef.current = meeting.id;
      setShowRecordingList(true);
      setElapsedSeconds(0);
      elapsedSecondsRef.current = 0;
      setStatus('recording');
      startTimers();
    } catch (recordingError) {
      setError(recordingError instanceof Error ? recordingError.message : 'Native recording could not start.');
    }
  };

  const handleStopRecording = async () => {
    setStatus('saving');
    stopTimers();
    try {
      const meeting = await stopNativeRecording();
      setMeetings((current) => upsertMeeting(current, meeting));
      setStatus('idle');
    } catch (stopError) {
      setError(stopError instanceof Error ? stopError.message : 'Failed to stop recording.');
      setStatus('recording');
      startTimers();
    }
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
    const modelPath = whisperModelPath.trim();
    if (!modelPath) {
      setError('Enter the full path to a local Whisper .bin model file.');
      return;
    }
    try {
      await loadWhisperModel(modelPath);
      setWhisperModelStatus('loaded');
    } catch (modelError) {
      setError(modelError instanceof Error ? modelError.message : String(modelError));
    }
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

        <main className="mini-transcript" aria-live="polite">
          <MessageSquareText size={15} />
          <p>{latestLine}</p>
        </main>

        {visibleTranscript.length > 1 && (
          <div className="mini-history">
            {visibleTranscript.slice(-3, -1).map((segment) => (
              <span key={segment.id}>{segment.text}</span>
            ))}
          </div>
        )}
      </div>
    );
  }

  return (
    <div className={`app-shell ${showRecordingList ? 'list-open' : ''}`}>
      <aside className="rail" aria-label="Primary navigation">
        <div className="brand-mark" title="Meetly Light">
          <span>M</span>
        </div>
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
        <div className="product-name">Meetly Light</div>
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
              onClick={() => setActiveMeetingId(meeting.id)}
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
            <button onClick={() => activeMeeting && handleExportTranscript(activeMeeting)} disabled={!activeMeeting} title="Export transcript" aria-label="Export transcript">
              <FileText size={16} />
            </button>
            <button onClick={() => activeMeeting && handleExportAudio(activeMeeting)} disabled={!activeMeeting?.hasAudio} title="Export audio" aria-label="Export audio">
              <Download size={16} />
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
              <span className="section-kicker">Meetly Light</span>
              <h2>Settings</h2>
              <p>Local transcription runs in the Rust backend. Configure the model before recording.</p>
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
                <span className={`model-status ${whisperModelStatus === 'loaded' ? 'loaded' : ''}`}>{whisperModelStatus}</span>
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
              <p>Load a Whisper model, then start recording from the rail or the floating control.</p>
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

              <div className="transcript-list" aria-live="polite">
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
          <div className="meter" aria-hidden="true">
            <span />
            <span />
            <span />
          </div>
        </div>
      </main>
    </div>
  );
}
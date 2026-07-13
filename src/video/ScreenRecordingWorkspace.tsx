import { CircleStop, FileText, FolderOpen, Mic, Monitor, MousePointer2, RefreshCw, Trash2, Volume2, Video } from 'lucide-react';
import type { ReactNode } from 'react';
import type { RecorderStatus, ScreenAudioLevels, ScreenProcessingStatus, ScreenTarget, VideoRecording } from '../types';
import { CAPTURE_MODE_OPTIONS } from '../audio/recordingOptions';
import { formatTimestamp } from '../exporters';

type Props = {
  status: RecorderStatus;
  elapsedSeconds: number;
  statusDisplay: ReactNode;
  targets: ScreenTarget[];
  selectedTarget: ScreenTarget | null;
  codec: 'h264' | 'h265';
  captureMode: string;
  audioLevels: ScreenAudioLevels;
  processingStatus: ScreenProcessingStatus | null;
  videos: VideoRecording[];
  onSelectScreen: (target: ScreenTarget) => void;
  onSelectArea: () => void;
  onCodecChange: (value: 'h264' | 'h265') => void;
  onCaptureModeChange: (value: string) => void;
  onStart: () => void;
  onStop: () => void;
  onOpenRecordingsFolder: () => void;
  onRefreshVideos: () => void;
  onGenerateTranscription: (video: VideoRecording) => void;
  onDeleteVideo: (videoId: string) => void;
  refreshingVideos: boolean;
  transcribing: boolean;
};

export function ScreenRecordingWorkspace({
  status, targets, selectedTarget, codec, videos, statusDisplay,
  captureMode, audioLevels, onSelectScreen, onSelectArea, onCodecChange, onCaptureModeChange, onStart, onStop, onOpenRecordingsFolder,
  processingStatus, onRefreshVideos, onGenerateTranscription, onDeleteVideo, refreshingVideos, transcribing,
}: Props) {
  const canChangeTarget = status === 'idle';
  const audioSources = [
    { label: 'System', icon: <Volume2 size={17} />, level: audioLevels.system, active: captureMode !== 'microphone' && captureMode !== 'none', tone: 'system' },
    { label: 'Microphone', icon: <Mic size={17} />, level: audioLevels.microphone, active: captureMode !== 'system' && captureMode !== 'none', tone: 'microphone' },
    { label: 'Mix', icon: <Volume2 size={17} />, level: audioLevels.mixed, active: captureMode !== 'none', tone: 'mix' },
  ];
  const videoStatusLabel = (video: VideoRecording) => {
    if (video.status === 'saved' && video.hasAudio === false) return 'Ready · No audio';
    if (video.status === 'saved') return 'Ready';
    if (video.status === 'recording') return 'Recording';
    if (video.status === 'post-processing') return 'Processing';
    if (video.status === 'capture-failed') return 'Capture failed';
    if (video.status === 'post-processing-failed') return 'Recovery pending';
    return video.status;
  };
  return (
    <section className="video-workspace" aria-label="Screen recording">
      <div className="video-panel">
        <div>
          <span className="section-kicker video-kicker">Screen recording</span>
          <h2>Capture your screen</h2>
          <p>Choose what to record, then save a compressed MP4 video.</p>
        </div>
        <div className="record-target-actions" aria-label="Recording target">
          <button type="button" onClick={() => targets[0] && onSelectScreen(targets[0])} disabled={!canChangeTarget || !targets[0]}>
            <Monitor size={18} /><span><strong>Record entire screen</strong><small>Capture all of your displays</small></span>
          </button>
          <button type="button" onClick={onSelectArea} disabled={!canChangeTarget}>
            <MousePointer2 size={18} /><span><strong>Select area</strong><small>Drag to choose part of your screen</small></span>
          </button>
        </div>
        {selectedTarget && <div className="selected-record-target"><span>Recording</span><strong>{selectedTarget.name}</strong></div>}
        <label>Video format
          <select value={codec} onChange={(event) => onCodecChange(event.target.value as 'h264' | 'h265')} disabled={!canChangeTarget}>
            <option value="h264">H.264</option>
            <option value="h265">H.265 (HEVC)</option>
          </select>
        </label>
        <label>Record audio
          <select value={captureMode} onChange={(event) => onCaptureModeChange(event.target.value)} disabled={!canChangeTarget}>
            <option value="none">No audio</option>
            {CAPTURE_MODE_OPTIONS.map((option) => <option key={option.value} value={option.value}>{option.label}</option>)}
          </select>
        </label>
        <div className={`screen-audio-mix ${status === 'recording' ? 'recording' : ''}`} aria-label="Screen recording audio mix">
          {audioSources.map((source) => <div className={`screen-audio-source ${source.tone} ${source.active ? '' : 'inactive'}`} key={source.label}>
            {source.icon}
            <div className="screen-audio-bars" aria-label={`${source.label} level`}>
              {Array.from({ length: 12 }, (_, index) => <i key={index} className={source.active && index < Math.ceil(source.level * 12) ? 'active' : ''} />)}
            </div>
            <span>{source.label}</span>
          </div>)}
        </div>
        <div className="screen-record-action">
          {statusDisplay}
          <button type="button" className="batch-submit" onClick={status === 'idle' ? onStart : onStop} disabled={status === 'starting' || status === 'saving' || !selectedTarget}>
            {status === 'idle' ? <><Video size={17} /> Start recording</> : <><CircleStop size={17} /> Stop recording</>}
          </button>
        </div>
        {processingStatus && <div className="video-processing-status" role="status">
          <strong>{processingStatus.stage}</strong>
          <span>{processingStatus.detail}</span>
        </div>}
      </div>
      <div className="video-list-panel">
        <div className="video-list-heading">
          <h3>Recorded videos</h3>
          <button type="button" className="video-folder-button" onClick={onOpenRecordingsFolder} title="Open recordings folder">
            <FolderOpen size={16} /> Open folder
          </button>
          <button type="button" className="video-icon-button" onClick={onRefreshVideos} disabled={refreshingVideos} title="Refresh recorded videos" aria-label="Refresh recorded videos">
            <RefreshCw size={16} className={refreshingVideos ? 'spinning' : ''} />
          </button>
        </div>
        {videos.map((video) => <article className="video-list-item" key={video.id}>
          <div><strong>{video.title}</strong><span>{new Date(video.createdAt).toLocaleString()} · {formatTimestamp(video.durationSeconds)} · {video.codec.toUpperCase()}</span><small className={`video-status ${video.status}`}>{videoStatusLabel(video)}</small>{video.status === 'capture-failed' && <span className="video-status-detail">No video frames were encoded. Select the area again and retry.</span>}{video.status === 'saved' && video.hasAudio === false && <span className="video-status-detail">{video.postProcessStage === 'audio-capture-failed' ? 'The video was saved, but screen audio capture failed.' : 'This recording was created without an audio track.'}</span>}</div>
          <div className="video-list-actions">
            <button type="button" className="video-icon-button" onClick={() => onGenerateTranscription(video)} disabled={video.status !== 'saved' || video.hasAudio === false || transcribing} title="Generate transcription" aria-label={`Generate transcription for ${video.title}`}>
              <FileText size={16} />
            </button>
            <button type="button" className="video-icon-button video-delete-button" onClick={() => onDeleteVideo(video.id)} disabled={video.status === 'recording' || video.status === 'post-processing'} title="Remove video" aria-label={`Remove ${video.title}`}>
              <Trash2 size={16} />
            </button>
          </div>
        </article>)}
        {videos.length === 0 && <p className="empty-list">No saved videos</p>}
      </div>
    </section>
  );
}

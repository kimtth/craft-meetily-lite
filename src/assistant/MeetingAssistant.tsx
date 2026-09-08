import { invoke, isTauri } from '@tauri-apps/api/core';
import { ChevronDown, Send, Sparkles, X } from 'lucide-react';
import { useEffect, useId, useLayoutEffect, useMemo, useRef, useState } from 'react';
import type { KeyboardEvent } from 'react';
import type { Meeting, TranscriptSegment } from '../types';
import './assistant.css';

// IPC contracts from copilot/schema.rs. Keep these private to this component.
type ContextCoverage = {
  contextSegmentCount?: number;
  // Maximum included segment START offset, never recording duration.
  contextThroughSeconds?: number;
};
type AssistantMessage = ContextCoverage & {
  id: string;
  role: 'user' | 'assistant';
  text: string;
  sourceIds: string[];
};
type Recap = {
  summary: string;
  decisions: { text: string; sourceIds: string[] }[];
  sourceIds: string[];
};
type ActionItem = {
  id: string;
  text: string;
  owner: string | null;
  due: string | null;
  done: boolean;
  sourceIds: string[];
};
type AssistantState = ContextCoverage & {
  messages: AssistantMessage[];
  recap?: Recap;
  actionItems: ActionItem[];
  transcriptFingerprint: string | null;
};
type CopilotStatus = {
  authenticated: boolean;
  detail: string;
  models: { id: string; name: string }[];
};
export type MeetingAssistantProps = {
  meeting: Meeting;
  onSeek: (offsetSeconds: number) => void;
  onClose?: () => void;
  recording?: boolean;
  playbackDisabled?: boolean;
  // Startup/save transitions only; recording itself must not disable chat.
  disabled?: boolean;
};
type Tab = 'recap' | 'actions' | 'chat';
const tabs: { id: Tab; label: string }[] = [
  { id: 'chat', label: 'Chat' },
  { id: 'recap', label: 'Recap' },
  { id: 'actions', label: 'Action items' },
];
const suggestions = [
  { label: 'Summarize so far', prompt: 'Summarize the meeting so far.' },
  { label: 'Decisions', prompt: 'What decisions have been made so far?' },
  { label: 'Action items', prompt: 'What are the action items, owners, and due dates mentioned so far?' },
  { label: 'Open questions', prompt: 'What questions are still unresolved?' },
];

// Pure, exported helpers can be transpiled for offline Node tests. This is only
// a conservative UI change detector, NOT a replacement for server prefix proof.
export function captureAssistantContext(meeting: Meeting) {
  const { transcript, updatedAt: _updatedAt, durationSeconds: _duration,
    hasAudio: _hasAudio, recordingPath: _path, ...metadata } = meeting;
  const ids = new Set<string>();
  const valid = transcript.every(segment => {
    if (!segment.id || ids.has(segment.id) || !Number.isFinite(segment.offsetSeconds) || segment.offsetSeconds < 0) return false;
    ids.add(segment.id);
    return true;
  });
  return { metadata: JSON.stringify(metadata), segments: transcript.map(segment => JSON.stringify(segment)), valid };
}

export function isAssistantContextAppend(before: ReturnType<typeof captureAssistantContext>, after: ReturnType<typeof captureAssistantContext>): boolean {
  return before.valid && after.valid && before.metadata === after.metadata
    && before.segments.length <= after.segments.length
    && before.segments.every((segment, index) => segment === after.segments[index]);
}

export function assistantCoverageLabel(coverage: ContextCoverage): string {
  const count = coverage.contextSegmentCount;
  const through = coverage.contextThroughSeconds;
  if (count == null || !Number.isSafeInteger(count) || count < 0
    || through == null || !Number.isFinite(through) || through < 0) return 'Transcript coverage unavailable';
  return `Transcript through ${timestamp(through)} · ${count} ${count === 1 ? 'segment' : 'segments'}`;
}

export function shouldSendAssistantKey(event: { key: string; shiftKey: boolean; isComposing: boolean; keyCode: number }): boolean {
  return event.key === 'Enter' && !event.shiftKey && !event.isComposing && event.keyCode !== 229;
}

function errorText(error: unknown): string {
  return typeof error === 'string' ? error : error instanceof Error ? error.message : 'The request failed. Please try again.';
}

function timestamp(offset: number): string {
  const seconds = Math.floor(offset);
  const hours = Math.floor(seconds / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  return `${hours ? `${hours}:` : ''}${String(minutes).padStart(2, '0')}:${String(seconds % 60).padStart(2, '0')}`;
}

function assistantTheme(): 'light' | 'dark' {
  // Theme detection is scoped to this panel; never mutate the app's <html>.
  const requested = new URLSearchParams(window.location.search).get('scoutTheme');
  if (requested === 'light' || requested === 'dark') return requested;
  const inherited = document.documentElement.getAttribute('data-theme');
  if (inherited === 'light' || inherited === 'dark') return inherited;
  return window.matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light';
}

export function MeetingAssistant(props: MeetingAssistantProps) {
  // Reset drafts, status and all view state even on an A → B → A switch.
  return <MeetingAssistantSession key={props.meeting.id} {...props} />;
}

export default MeetingAssistant;

function MeetingAssistantSession({ meeting, onSeek, onClose, recording = false, playbackDisabled = false, disabled = false }: MeetingAssistantProps) {
  const prefix = useId();
  const [theme, setTheme] = useState(assistantTheme);
  const [tab, setTab] = useState<Tab>('chat');
  const [settingsExpanded, setSettingsExpanded] = useState(true);
  const [question, setQuestion] = useState('');
  const [status, setStatus] = useState<CopilotStatus | null>(null);
  const [model, setModel] = useState('');
  const [snapshot, setSnapshot] = useState<{ context: ReturnType<typeof captureAssistantContext>; data: AssistantState } | null>(null);
  const [pendingPrompt, setPendingPrompt] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState('');
  const [operation, setOperation] = useState<'status' | 'recap' | 'chat' | 'done' | null>(null);
  const [reload, setReload] = useState(0);
  const alive = useRef(false);
  const locked = useRef(false);
  const localRead = useRef<{ revision: number; context: ReturnType<typeof captureAssistantContext>; promise: Promise<AssistantState> } | null>(null);
  const tabButtons = useRef<(HTMLButtonElement | null)[]>([]);
  const composer = useRef<HTMLTextAreaElement>(null);
  const messagesScroll = useRef<HTMLDivElement>(null);
  const followLatest = useRef(true);

  const context = captureAssistantContext(meeting);
  const currentContext = useRef(context);
  useLayoutEffect(() => { currentContext.current = context; }, [context]);
  useLayoutEffect(() => {
    alive.current = true;
    return () => { alive.current = false; };
  }, []);

  useEffect(() => {
    const update = () => setTheme(assistantTheme());
    const media = window.matchMedia('(prefers-color-scheme: dark)');
    const observer = new MutationObserver(update);
    observer.observe(document.documentElement, { attributes: true, attributeFilter: ['data-theme'] });
    media.addEventListener('change', update);
    return () => { observer.disconnect(); media.removeEventListener('change', update); };
  }, []);

  useEffect(() => {
    // Only this local storage read is automatic. No status, auth, model discovery
    // or inference is launched by an effect. Reuse the promise under StrictMode;
    // never re-read on a transcript append or when an operation finishes.
    let cancelled = false;
    const isCurrent = () => !cancelled && alive.current;
    setLoading(true);
    setLoadError(null);
    if (!isTauri()) {
      setLoadError('Meeting assistant requires the Meetly desktop app. Browser preview does not connect to Copilot.');
      setLoading(false);
      locked.current = false;
      return () => { cancelled = true; };
    }
    if (!localRead.current || localRead.current.revision !== reload) {
      localRead.current = { revision: reload, context: currentContext.current,
        promise: invoke<AssistantState>('get_meeting_assistant', { meetingId: meeting.id }) };
    }
    const request = localRead.current;
    void request.promise.then(data => {
      if (isCurrent()) setSnapshot({ context: request.context, data });
    }).catch((reason: unknown) => {
      if (isCurrent()) setLoadError(errorText(reason));
    }).finally(() => {
      if (isCurrent()) { setLoading(false); locked.current = false; }
    });
    return () => { cancelled = true; };
  }, [meeting.id, reload]);

  const sources = useMemo(() => {
    const result = new Map<string, TranscriptSegment | null>();
    for (const segment of meeting.transcript) {
      result.set(segment.id, result.has(segment.id) ? null : segment);
    }
    return result;
  }, [meeting.transcript]);
  const data = snapshot?.data;
  const hasSavedContent = !!(data?.recap || data?.messages.length || data?.actionItems.length);
  const stale = hasSavedContent && (data?.transcriptFingerprint == null
    || !snapshot || !isAssistantContextAppend(snapshot.context, context));
  const busy = operation !== null;
  const ready = !disabled && !busy && !loading && !loadError && !!snapshot;
  const hasTranscript = meeting.transcript.some(segment => segment.text.trim().length > 0);
  const selectedModel = status?.models.find(candidate => candidate.id === model);
  const configured = !!status?.authenticated && !!selectedModel;
  const setupOpen = settingsExpanded;
  const canInfer = ready && hasTranscript && configured;
  const questionBytes = new TextEncoder().encode(question.trim()).length;
  const canSend = canInfer && questionBytes > 0 && questionBytes <= 4000;
  const newSegments = !stale && data?.contextSegmentCount != null
    ? Math.max(0, meeting.transcript.length - data.contextSegmentCount) : 0;

  useLayoutEffect(() => {
    const region = messagesScroll.current;
    if (region && tab === 'chat' && followLatest.current) region.scrollTop = region.scrollHeight;
  }, [data?.messages, pendingPrompt, tab]);

  // A synchronous lock closes the double-click window before React re-renders.
  function begin(next: NonNullable<typeof operation>): boolean {
    if (locked.current || disabled || loading) return false;
    locked.current = true;
    setOperation(next);
    setError(null);
    setNotice('');
    return true;
  }

  function finish() {
    if (!alive.current) return;
    locked.current = false;
    setPendingPrompt(null);
    setOperation(null);
  }

  async function checkStatus() {
    if (!begin('status')) return;
    const meetingId = meeting.id;
    try {
      if (!isTauri()) throw new Error('Open the Meetly desktop app to connect to Copilot.');
      const result = await invoke<CopilotStatus>('copilot_status');
      if (!alive.current || meeting.id !== meetingId) return;
      setStatus(result);
      setModel(previous => result.models.some(candidate => candidate.id === previous)
        ? previous : result.models[0]?.id ?? '');
    } catch (reason) {
      if (alive.current && meeting.id === meetingId) {
        setStatus(null);
        setModel('');
        setError(errorText(reason));
      }
    } finally { finish(); }
  }

  async function ask(kind: 'recap' | 'chat') {
    if (!canInfer || (kind === 'chat' && !canSend) || !begin(kind)) return;
    const meetingId = meeting.id;
    const requestContext = context;
    const prompt = kind === 'chat' ? question.trim() : '';
    const draft = question;
    const chosenModel = model;
    if (kind === 'chat') { followLatest.current = true; setPendingPrompt(prompt); }
    try {
      const result = await invoke<AssistantState>('ask_copilot', {
        // Explicit submission authorizes sharing; opening the pane never does.
        meetingId, prompt, kind, model: chosenModel, consent: true,
      });
      // The keyed session, not transcript equality, owns this response. The
      // server certifies its entry snapshot and tolerates same-meeting appends.
      if (!alive.current) return;
      setSnapshot({ context: requestContext, data: result });
      if (kind === 'chat') setQuestion(value => value === draft ? '' : value);
      setNotice(`${kind === 'recap' ? 'Recap and action items' : 'Answer'} saved locally.`);
    } catch (reason) {
      if (alive.current) setError(errorText(reason));
    } finally { finish(); }
  }

  async function toggleAction(action: ActionItem) {
    if (!ready || !begin('done')) return;
    const meetingId = meeting.id;
    const requestContext = context;
    try {
      const result = await invoke<AssistantState>('set_action_item_done', {
        meetingId, actionId: action.id, done: !action.done,
      });
      if (!alive.current) return;
      setSnapshot({ context: requestContext, data: result });
      setNotice('Action item completion saved locally.');
    } catch (reason) {
      if (alive.current) setError(errorText(reason));
    } finally { finish(); }
  }

  function references(ids: string[]) {
    if (!ids.length) return null;
    return <ul className="ma-references" aria-label="Transcript references">
      {[...new Set(ids)].map(id => {
        const segment = sources.get(id);
        if (!segment || !Number.isFinite(segment.offsetSeconds) || segment.offsetSeconds < 0) {
          return <li key={id}><span className="ma-muted">Reference unavailable in current transcript</span></li>;
        }
        return <li key={id}><button type="button" className="ma-reference" disabled={disabled || playbackDisabled || stale}
          title={segment.text} aria-label={`Seek to ${timestamp(segment.offsetSeconds)}: ${segment.text}`}
          onClick={() => onSeek(segment.offsetSeconds)}>{timestamp(segment.offsetSeconds)}</button></li>;
      })}
    </ul>;
  }

  function moveTab(event: KeyboardEvent<HTMLButtonElement>, index: number) {
    let next: number;
    if (event.key === 'ArrowRight') next = (index + 1) % tabs.length;
    else if (event.key === 'ArrowLeft') next = (index + tabs.length - 1) % tabs.length;
    else if (event.key === 'Home') next = 0;
    else if (event.key === 'End') next = tabs.length - 1;
    else return;
    event.preventDefault();
    setTab(tabs[next].id);
    tabButtons.current[next]?.focus();
  }

  return <section className="meeting-assistant" data-theme={theme} aria-labelledby={`${prefix}-heading`}>
    <header className="ma-heading">
      <div className="ma-brand"><Sparkles size={20} aria-hidden="true" /><h2 id={`${prefix}-heading`}>Copilot</h2></div>
      {recording && <span className="ma-live"><span aria-hidden="true" />Recording</span>}
      {onClose && <button type="button" className="ma-icon" onClick={onClose} aria-label="Close Copilot pane" title="Close Copilot pane"><X size={18} aria-hidden="true" /></button>}
    </header>

    <div className="ma-context">
      <p className="ma-meeting-title" title={meeting.title}>{meeting.title || 'Untitled meeting'}</p>
      <p className="ma-muted">{meeting.transcript.length} transcript {meeting.transcript.length === 1 ? 'segment' : 'segments'} available
        {newSegments > 0 && <span className="ma-growth">+{newSegments} since last answer</span>}</p>
    </div>

    <div className="ma-setup">
      <button type="button" className="ma-setup-toggle" aria-expanded={setupOpen} aria-controls={`${prefix}-setup`}
        onClick={() => setSettingsExpanded(value => !value)}>
        <span>{configured ? `Ready · ${selectedModel?.name}` : 'Set up Copilot for this meeting'}</span>
        <ChevronDown size={16} className={setupOpen ? 'ma-rotated' : ''} aria-hidden="true" />
      </button>
      <div id={`${prefix}-setup`} className="ma-connection" hidden={!setupOpen}>
        <div className="ma-toolbar">
          <button type="button" disabled={disabled || busy || loading} onClick={() => void checkStatus()}>
            {operation === 'status' ? 'Checking…' : 'Check Copilot status'}
          </button>
          <button type="button" disabled={disabled || busy || loading} onClick={() => {
            if (locked.current) return;
            locked.current = true;
            setLoading(true); setError(null); setNotice(''); setReload(value => value + 1);
          }}>Reload saved state</button>
        </div>
        <p className="ma-muted">Status checks may contact GitHub for authentication and models. They do not send your transcript or generate answers.</p>
        {status && <p className="ma-muted"><strong>{status.authenticated ? 'Authenticated' : 'Unavailable'}</strong> — {status.detail}</p>}
        <label className="ma-model" htmlFor={`${prefix}-model`}>Model for next request
          <select id={`${prefix}-model`} value={model} disabled={disabled || busy || !status?.models.length}
            onChange={event => setModel(event.target.value)}>
            {!status?.models.length && <option value="">Check status to discover models</option>}
            {status?.models.map(candidate => <option key={candidate.id} value={candidate.id}>{candidate.name} ({candidate.id})</option>)}
          </select>
        </label>
        <p className="ma-muted">Send or Generate sends this meeting’s transcript, question and assistant history to GitHub Copilot. Nothing is sent automatically.</p>
        <p className="ma-muted">Copilot usage may incur charges or consume your allowance. Long transcripts may need multiple paid requests and at most one validation retry per submission.</p>
        <p className="ma-muted">History is saved locally. No automatic authentication or generation. Saved answers do not record the model selection. Closing or switching meetings does not cancel an in-flight request.</p>
      </div>
    </div>

    <div className="ma-tabs" role="tablist" aria-label="Meeting assistant views">
      {tabs.map((item, index) => <button type="button" role="tab" key={item.id}
        id={`${prefix}-tab-${item.id}`} aria-controls={`${prefix}-panel-${item.id}`}
        aria-selected={tab === item.id} tabIndex={tab === item.id ? 0 : -1}
        ref={element => { tabButtons.current[index] = element; }}
        onKeyDown={event => moveTab(event, index)} onClick={() => setTab(item.id)}>{item.label}</button>)}
    </div>

    <div className="ma-feedback" role="status" aria-live="polite" aria-atomic="true">
      {operation === 'recap' ? 'Generating recap and actions from a fixed transcript snapshot…'
        : operation === 'done' ? 'Saving completion locally…'
          : operation === 'status' ? 'Checking authentication and models…'
            : loading ? 'Loading local history…' : notice}
    </div>
    {(error || loadError) && <div className="ma-error" role="alert">
      {error && <p>{error}{question && ' Your draft is kept; retry when ready.'}</p>}
      {loadError && <p>{loadError} Use Reload saved state to retry.</p>}
    </div>}

    <div className="ma-panel" id={`${prefix}-panel-recap`} role="tabpanel" aria-labelledby={`${prefix}-tab-recap`} tabIndex={0} hidden={tab !== 'recap'} aria-busy={loading || operation === 'recap'}>
      <div className="ma-toolbar"><button type="button" className="ma-primary" disabled={!canInfer} onClick={() => void ask('recap')}>
        {operation === 'recap' ? 'Generating…' : data?.recap ? 'Regenerate recap & actions' : 'Generate recap & actions'}
      </button></div>
      <p className="ma-muted">Recap and actions are saved snapshots, not live updates. Latest-request coverage in Chat does not necessarily describe this recap.</p>
      {stale && <p className="ma-notice">Saved content is historical and not certified for the current transcript. Generate to refresh it, or ask directly in Chat.</p>}
      {data?.recap ? <>
        <h3>Summary</h3><p className="ma-prose">{data.recap.summary}</p>{references(data.recap.sourceIds)}
        <h3>Decisions</h3>
        {data.recap.decisions.length ? <ul className="ma-cards">{data.recap.decisions.map((decision, index) =>
          <li key={index}><p className="ma-prose">{decision.text}</p>{references(decision.sourceIds)}</li>)}</ul>
          : <p className="ma-empty">No supported decisions were returned in this recap.</p>}
      </> : <p className="ma-empty">{loading ? 'Loading recap…' : loadError ? 'Saved recap is unavailable.' : 'No saved recap. Check status, select a model, then choose Generate recap & actions.'}</p>}
    </div>

    <div className="ma-panel" id={`${prefix}-panel-actions`} role="tabpanel" aria-labelledby={`${prefix}-tab-actions`} tabIndex={0} hidden={tab !== 'actions'} aria-busy={loading || operation === 'done' || operation === 'recap'}>
      <h3>Action items</h3>
      <p className="ma-muted">Completion changes are saved locally without a cloud request.</p>
      {stale && <p className="ma-notice">These historical items are not certified for the current transcript.</p>}
      <div className="ma-toolbar"><button type="button" disabled={!canInfer} onClick={() => void ask('recap')}>
        {operation === 'recap' ? 'Generating…' : 'Generate recap & actions'}
      </button></div>
      {data?.actionItems.length ? <>
        <p className="ma-muted">{data.actionItems.filter(action => action.done).length} of {data.actionItems.length} complete</p>
        <ul className="ma-cards">{data.actionItems.map(action => <li key={action.id}>
          <label className="ma-action"><input type="checkbox" checked={action.done} disabled={!ready}
            onChange={() => void toggleAction(action)} /><span className={action.done ? 'ma-completed ma-prose' : 'ma-prose'}>{action.text}</span></label>
          <dl className="ma-action-meta"><div><dt>Owner</dt><dd>{action.owner ?? 'Not specified'}</dd></div>
            <div><dt>Due</dt><dd>{action.due ?? 'Not specified'}</dd></div></dl>
          {references(action.sourceIds)}
        </li>)}</ul>
      </> : <p className="ma-empty">{loading ? 'Loading action items…' : loadError ? 'Saved action items are unavailable.' : data?.recap ? 'No supported action items were returned in this recap.' : 'Generate a recap to extract supported action items.'}</p>}
    </div>

    <div className="ma-panel ma-chat" id={`${prefix}-panel-chat`} role="tabpanel" aria-labelledby={`${prefix}-tab-chat`} hidden={tab !== 'chat'}>
      <div className="ma-chat-scroll" ref={messagesScroll} tabIndex={0} role="region" aria-label="Conversation history"
        onScroll={event => { const el = event.currentTarget; followLatest.current = el.scrollHeight - el.scrollTop - el.clientHeight < 48; }}>
        <p className="ma-muted">Ask about this meeting, then follow up. Answers use a fixed transcript snapshot; new speech is available on your next request. Verify answers using sources.</p>
        {hasSavedContent && <p className="ma-coverage" title="Latest successful request. Time is the maximum segment start, not audio duration.">{assistantCoverageLabel(data!)}</p>}
        {stale && <p className="ma-notice">History is not certified for this transcript version. You can still ask a new question; the server excludes unsafe history. No recap is required.</p>}
        {!data?.messages.length && !pendingPrompt && <div className="ma-welcome">
          <Sparkles size={28} aria-hidden="true" /><h3>Stay in the conversation</h3>
          <p>{loading ? 'Loading your local conversation…' : loadError ? 'Local conversation is unavailable.' : 'Catch up, find decisions, or ask what comes next—even while recording.'}</p>
          <p className="ma-muted">Choose a prompt below to edit before sending.</p>
        </div>}
        <ol className="ma-messages" aria-label="Meeting conversation">{data?.messages.map(message =>
          <li key={message.id} className={`ma-message ma-message-${message.role}`}><strong>{message.role === 'user' ? 'You' : 'Copilot'}</strong>
            <p className="ma-prose">{message.text}</p>
            {message.role === 'assistant' && <p className="ma-coverage" title="Maximum included segment start, not audio duration.">{assistantCoverageLabel(message)}</p>}
            {references(message.sourceIds)}</li>)}
          {pendingPrompt && <>
            <li className="ma-message ma-message-user ma-pending"><strong>You · Sending</strong><p className="ma-prose">{pendingPrompt}</p></li>
            <li className="ma-message ma-message-assistant ma-thinking" role="status" aria-live="polite"><strong>Copilot is thinking…</strong>
              <p>Waiting for a validated, saved answer. This may take several minutes. Recording can continue.</p></li>
          </>}
        </ol>
      </div>
      <form className="ma-composer" onSubmit={event => { event.preventDefault(); void ask('chat'); }}>
        <div className="ma-suggestions" aria-label="Suggested prompts">{suggestions.map(suggestion =>
          <button key={suggestion.label} type="button" disabled={disabled || busy} onClick={() => {
            setQuestion(suggestion.prompt); setError(null); composer.current?.focus();
          }}>{suggestion.label}</button>)}</div>
        <label className="ma-sr-only" htmlFor={`${prefix}-question`}>Question about this meeting</label>
        <textarea ref={composer} id={`${prefix}-question`} rows={2} value={question} disabled={disabled} readOnly={busy}
          placeholder={data?.messages.length ? 'Ask a follow-up…' : 'Ask about this meeting…'}
          aria-describedby={`${prefix}-question-help`} aria-invalid={questionBytes > 4000}
          onChange={event => setQuestion(event.target.value)} onKeyDown={event => {
            if (shouldSendAssistantKey({ key: event.key, shiftKey: event.shiftKey,
              isComposing: event.nativeEvent.isComposing, keyCode: event.nativeEvent.keyCode })) {
              event.preventDefault();
              if (canSend) void ask('chat');
            }
          }} />
        <div className="ma-composer-actions">
          <p id={`${prefix}-question-help`} className={questionBytes > 4000 ? 'ma-invalid' : 'ma-muted'}>
            Enter to send · Shift+Enter for a new line<br />{questionBytes.toLocaleString()} / 4,000 UTF-8 bytes
          </p>
          <button type="submit" className="ma-primary" disabled={!canSend} aria-label="Send to Copilot"><Send size={16} aria-hidden="true" />Send</button>
        </div>
        <p className="ma-composer-hint">{disabled ? 'Temporarily unavailable during startup or saving.'
          : !hasTranscript ? 'Waiting for confirmed transcript text.'
            : !configured ? 'Check status and select a model above to send.'
              : 'AI answers may be inaccurate. Copilot usage may incur charges.'}</p>
      </form>
    </div>
  </section>;
}
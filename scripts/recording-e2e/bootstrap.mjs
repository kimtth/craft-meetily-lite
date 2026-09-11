// This module precedes the real src/main.tsx, including React StrictMode.
import { mockIPC, mockWindows } from '@tauri-apps/api/mocks';
import { emit } from '@tauri-apps/api/event';

const gate = () => {
  let resolve;
  const promise = new Promise((done) => { resolve = done; });
  return { promise, resolve };
};
const settingsGate = gate();
const catalogGate = gate();
const persistenceGate = gate();
let runtimeStatusGate = null;
const response = await fetch('/__recording-e2e/wire');
if (!response.ok) throw new Error('Cannot load captured wire');
const wire = new Uint8Array(await response.arrayBuffer());
const records = new TextDecoder('utf-8', { fatal: true }).decode(wire).trim().split(/\r?\n/).map(JSON.parse);
const wireHash = [...new Uint8Array(await crypto.subtle.digest('SHA-256', wire))].map((n) => n.toString(16).padStart(2, '0')).join('');
const key = 'meetly-lite:recording-e2e:store';
const meeting = (id, title) => ({
  id, title, createdAt: '2026-09-11T00:00:00Z', updatedAt: '2026-09-11T00:00:00Z',
  durationSeconds: 0, transcript: [], hasAudio: false, captureMode: 'system',
  language: 'en-US', transcriptionEngine: 'azure',
});
if (!localStorage.getItem(key)) {
  localStorage.setItem(key, JSON.stringify([
    meeting('synthetic-wire-a', 'Synthetic Wire Session A'),
    meeting('synthetic-wire-b', 'Synthetic Wire Session B'),
  ]));
}
const load = () => JSON.parse(localStorage.getItem(key));
const save = (meetings) => localStorage.setItem(key, JSON.stringify(meetings));
const state = window.__recordingE2E = {
  wireHash, wireBytes: wire.length, wireCount: records.length,
  calls: [], log: [], speech: [], delivered: [], failures: [], activeId: null,
  replayCursor: 0, settingsWaiting: false, catalogWaiting: false,
  catalogResolved: false, persistenceWaiting: false,
  runtimeStatusPending: 0, runtimeStatusResolved: 0, runtimeStatusHeld: 0,
  holdRuntimeStatus: () => {
    if (!state.catalogWaiting || !state.runtimeStatusResolved || state.runtimeStatusPending || state.activeId
      || state.calls.some((call) => /^(start|stop)_recording$/.test(call.command))) {
      throw new Error('Runtime status may only be held after idle startup, without any capture/stop');
    }
    if (runtimeStatusGate) throw new Error('Runtime status is already held');
    runtimeStatusGate = gate();
    state.log.push('runtimeStatus.hold');
  },
  releaseRuntimeStatus: () => {
    runtimeStatusGate?.resolve();
    runtimeStatusGate = null;
    state.log.push('runtimeStatus.release');
  },
  releaseSettings: () => settingsGate.resolve(),
  releaseCatalog: () => { state.catalogResolved = true; catalogGate.resolve(); },
  releasePersistence: () => persistenceGate.resolve(),
  stored: load,
};
const deliver = async (index) => {
  if (index !== state.replayCursor) throw new Error(`Out-of-order replay ${index}`);
  const record = records[index];
  state.replayCursor++;
  // Only the identity is replaced. Base64 and offset fields stay exact.
  await emit('audio-chunk', { ...record, meetingId: state.activeId });
};
state.replay = async (speed = 1) => {
  const start = performance.now();
  // Native stop supplies the last record, not the test driver's replay loop.
  for (let index = state.replayCursor; index < records.length - 1; index++) {
    const due = speed > 0 ? records[index].offsetSeconds * 1000 / speed : 0;
    const delay = due - (performance.now() - start);
    if (delay > 0) await new Promise((resolve) => setTimeout(resolve, delay));
    await deliver(index);
  }
};

const defaultSettings = {
  transcriptionEngine: 'foundryLocal', captureMode: 'microphoneSystem', screenAudioCaptureMode: 'system',
  audioDeviceId: '', systemAudioDeviceId: '', language: 'en', azureEndpoint: '', azureTenantId: '',
  azureSubscriptionId: '', azureLanguage: 'en-US', foundryLocalModelAlias: '', foundryLocalLanguage: 'en',
  foundryLocalChunkingMode: 'utterance', videoOutputFolder: '', videoCodec: 'h264', ffmpegPath: '',
};
const settingsKey = `${key}:settings`;
const handlers = {
  get_meetings: () => load(),
  refresh_meetings: () => load(),
  refresh_videos: () => [],
  list_screen_targets: () => [],
  get_settings: async () => {
    state.settingsWaiting = true;
    await settingsGate.promise;
    return JSON.parse(localStorage.getItem(settingsKey) || JSON.stringify(defaultSettings));
  },
  save_settings: ({ settings }) => localStorage.setItem(settingsKey, JSON.stringify(settings)),
  list_audio_input_devices: () => [{ id: '', name: 'Synthetic input (no device access)', isDefault: true }],
  list_audio_output_devices: () => [{ id: '', name: 'Captured WASAPI wire (no device access)', isDefault: true }],
  check_azure_cli_sign_in: () => ({ id: 'azure', state: 'connected', detail: 'SYNTHETIC offline sign-in fixture' }),
  list_foundry_local_models: async () => {
    state.catalogWaiting = true;
    await catalogGate.promise;
    return [];
  },
  get_recording_runtime_status: async () => {
    state.runtimeStatusPending++;
    try {
      if (runtimeStatusGate) {
        state.runtimeStatusHeld++;
        await runtimeStatusGate.promise;
      }
      state.runtimeStatusResolved++;
      return {
        audioRecordingActive: !!state.activeId, audioMeetingId: state.activeId,
        audioElapsedSeconds: 0, audioMicrophoneMuted: false, screenRecordingActive: false, screenElapsedSeconds: 0,
      };
    } finally {
      state.runtimeStatusPending--;
    }
  },
  start_recording: async (args) => {
    if (state.activeId || args.transcriptionEngine !== 'azure' || args.captureMode !== 'system') {
      throw new Error('Unexpected recording options or duplicate start');
    }
    const current = load().find((item) => item.id === args.appendToMeetingId);
    if (!current) throw new Error('Expected explicit append to a synthetic meeting');
    state.activeId = current.id;
    state.log.push('native.start');
    // Actual early event before IPC returns exercises App's startup buffer.
    await deliver(0);
    return current;
  },
  stop_recording: async () => {
    if (!state.activeId || state.replayCursor !== records.length - 1) throw new Error('Stop before replay finished or duplicate stop');
    state.log.push('native.stop');
    await deliver(records.length - 1);
    state.log.push('native.tailDelivered');
    const meetings = load();
    const current = meetings.find((item) => item.id === state.activeId);
    // Like native persistence, derive duration once from integer sample count,
    // not by adding floating point chunk durations.
    current.durationSeconds = records.reduce((bytes, record) => bytes + atob(record.pcmBase64).length, 0) / 32000;
    current.hasAudio = true;
    save(meetings);
    state.activeId = null;
    state.log.push('native.stopReturned');
    return structuredClone(current);
  },
  add_transcript_segment: async ({ meetingId, text, offsetSeconds }) => {
    if (!text.startsWith('SYNTHETIC ')) throw new Error('Only synthetic recognition is permitted');
    if (text.startsWith('SYNTHETIC final')) {
      state.log.push('persistence.waiting');
      state.persistenceWaiting = true;
      await persistenceGate.promise;
    }
    const meetings = load();
    const current = meetings.find((item) => item.id === meetingId);
    if (!current) throw new Error('Persistence targeted a non-fixture meeting');
    const segment = { id: `synthetic-${current.transcript.length}`, text, offsetSeconds };
    current.transcript.push(segment);
    save(meetings);
    state.log.push(text.startsWith('SYNTHETIC final') ? 'persistence.finalSaved' : 'persistence.earlySaved');
    await emit('transcript-segment', { meetingId, segment });
    return segment;
  },
};
// Current Tauri core.isTauri() tests this marker, not __TAURI_INTERNALS__.
window.isTauri = true;
mockIPC(async (command, args) => {
  state.calls.push({ command, args });
  if (!Object.hasOwn(handlers, command)) {
    state.failures.push(`Denied IPC: ${command}`);
    throw new Error(`Denied IPC: ${command}`);
  }
  try { return await handlers[command](args); }
  catch (error) { state.failures.push(String(error)); throw error; }
}, { shouldMockEvents: true });
mockWindows('main');

const events = new Set([
  'audio-chunk', 'transcript-segment', 'recording-error', 'transcription-error',
  'microphone-mute-changed', 'call-mute-warning', 'fast-transcription-progress',
  'area-selected', 'screen-audio-level', 'screen-processing-status', 'foundry-download-progress',
]);
const internals = window.__TAURI_INTERNALS__;
const invoke = internals.invoke;
internals.invoke = (command, args, options) => {
  if (command.startsWith('plugin:event|')) {
    if (!['plugin:event|listen', 'plugin:event|emit', 'plugin:event|unlisten'].includes(command) || !events.has(args.event)) {
      state.failures.push(`Denied event IPC: ${command}/${args.event}`);
      return Promise.reject(new Error('Denied event IPC'));
    }
  }
  return invoke(command, args, options);
};
const transform = internals.transformCallback;
internals.transformCallback = (callback, once) => transform((event) => {
  // This witness sits at the real Tauri JS listener callback, not App internals.
  if (event?.event === 'audio-chunk') state.delivered.push(structuredClone(event.payload));
  return callback(event);
}, once);

await import('/src/main.tsx');
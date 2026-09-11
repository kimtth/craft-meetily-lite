import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import { runInNewContext } from 'node:vm';
import React from 'react';
import ts from 'typescript';

// Like azureSession.test.mjs, execute production TypeScript in memory. Only
// named declarations and actual RecordButton call sites are extracted: no App
// imports, mount effects, devices, credentials, network, or meeting data run.
// AST scope/uniqueness assertions deliberately fail if App is reorganized.
const source = await readFile(new URL('../App.tsx', import.meta.url), 'utf8');
const ast = ts.createSourceFile('App.tsx', source, ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
assert.equal(ast.parseDiagnostics.length, 0, 'App.tsx must parse before extracting controls');

function declaration(statements, name) {
  const matches = [];
  for (const statement of statements) {
    if (ts.isFunctionDeclaration(statement) && statement.name?.text === name) {
      matches.push(statement);
    } else if (ts.isVariableStatement(statement)) {
      for (const item of statement.declarationList.declarations) {
        if (ts.isIdentifier(item.name) && item.name.text === name) matches.push(item);
      }
    }
  }
  assert.equal(matches.length, 1, `Expected exactly one declaration of ${name} in its original scope`);
  return matches[0];
}

const app = declaration(ast.statements, 'App');
assert.ok(ts.isFunctionDeclaration(app) && app.body);
const topNames = ['RecordButton', 'getErrorMessage', 'upsertMeeting'];
const handlerNames = [
  'beginRecordingAttempt', 'identifyRecordingAttempt', 'receiveRecordingFailure',
  'surfaceRecordingFailure', 'revokeFastConsent', 'enqueueRecordingOperation',
  'closeAzurePipe', 'startTimers', 'stopTimers', 'handleStopRecording',
  'stopRecordingAttempt', 'handleSelectMeeting', 'handleCancelSwitch',
];
const declarations = new Map([
  ...topNames.map((name) => [name, declaration(ast.statements, name)]),
  ...handlerNames.map((name) => [name, declaration(app.body.statements, name)]),
]);
const buttonSites = [];
function visit(node) {
  if ((ts.isJsxSelfClosingElement(node) || ts.isJsxElement(node))
    && (ts.isJsxSelfClosingElement(node) ? node.tagName : node.openingElement.tagName).getText(ast) === 'RecordButton') {
    buttonSites.push(node.getText(ast));
  }
  ts.forEachChild(node, visit);
}
visit(app.body);
assert.equal(buttonSites.length, 2, 'Exercise both actual mini and floating recording controls');

function compile({ stubStop = false } = {}) {
  const names = [...declarations.keys()].filter((name) => !stubStop || name !== 'stopRecordingAttempt');
  const code = names.map((name) => {
    const node = declarations.get(name);
    return ts.isVariableDeclaration(node) ? `const ${node.getText(ast)};` : node.getText(ast);
  }).join('\n');
  const result = ts.transpileModule(`${code}\nexport { ${names.join(', ')} };
    export const recordButtons = () => [${buttonSites.join(',\n')}];`, {
    fileName: 'recordingControls.extracted.tsx',
    compilerOptions: {
      target: ts.ScriptTarget.ES2020,
      module: ts.ModuleKind.CommonJS,
      jsx: ts.JsxEmit.React,
    },
    reportDiagnostics: true,
  });
  assert.deepEqual(result.diagnostics.filter((d) => d.category === ts.DiagnosticCategory.Error), []);
  return result.outputText;
}
const controlCode = compile();
const delegationCode = compile({ stubStop: true });
const flush = () => new Promise((resolve) => setImmediate(resolve));
function deferred() {
  let resolve;
  let reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
const attemptFor = (meetingId) => ({ meetingId, pendingErrors: new Map(), failure: null });
const inactive = () => ({ audioRecordingActive: false, audioMeetingId: null });
const active = (audioMeetingId = 'live') => ({ audioRecordingActive: true, audioMeetingId });

// Node has Event but no DOM MouseEvent. This explicit offline event double is
// passed to the REAL button's onClick, just as React supplies a mouse event.
// It is intentionally not an attempt identity and must never reach stop's guard.
class MouseEvent extends Event {
  constructor() {
    super('click', { bubbles: true, cancelable: true });
    this.button = 0;
    this.clientX = 10;
    this.clientY = 20;
    this.nativeEvent = new Event('click');
  }
}

function harness({ status = 'idle', attempt = null, recordingId = null, runtime = inactive, delegate } = {}) {
  const calls = [];
  const timers = new Map();
  let timerId = 0;
  const state = {
    status, activeMeetingId: 'selected', pendingMeetingId: null, error: null,
    meetings: [], recordingOperationBusy: false, fastUploadConsent: true,
    elapsedSeconds: 0, azurePartialText: 'synthetic partial', microphoneMuted: true,
    showSettings: false, showBatchTranscription: false, showVideoWorkspace: false,
  };
  const globals = { React, Error };
  const refs = {};
  for (const [name, current] of Object.entries({
    recordingAttemptRef: attempt, recordingMeetingIdRef: recordingId,
    recordingOperationsRef: 0, recordingQueueRef: Promise.resolve(),
    stopRecordingPromiseRef: null, meetingSelectionRequestRef: 0,
    azurePipeRef: null, elapsedTimerRef: null, elapsedSecondsRef: 0,
    fastConsentRef: { synthetic: true },
  })) {
    refs[name] = { current };
    globals[name] = refs[name];
  }
  for (const name of Object.keys(state)) {
    Object.defineProperty(globals, name, { get: () => state[name], configurable: true });
    globals[`set${name[0].toUpperCase()}${name.slice(1)}`] = (value) => {
      state[name] = typeof value === 'function' ? value(state[name]) : value;
      calls.push([name, state[name]]);
    };
  }
  Object.assign(globals, {
    exports: {},
    require(name) { throw new Error(`Unexpected import in offline controls test: ${name}`); },
    fetch() { throw new Error('Network is forbidden in recording controls tests'); },
    Mic: () => null,
    CircleStop: () => null,
    handleOpenRecordingChoice() { calls.push(['start.choice']); },
    window: {
      setInterval(callback, milliseconds) {
        const id = ++timerId;
        timers.set(id, { callback, milliseconds });
        calls.push(['timer.start']);
        return id;
      },
      clearInterval(id) { timers.delete(id); calls.push(['timer.stop']); },
    },
    async getRecordingRuntimeStatus() { calls.push(['runtime']); return runtime(); },
    async stopNativeRecording() {
      calls.push(['native.stop']);
      return { id: 'live', transcript: [], updatedAt: '2026-01-01T00:00:00Z' };
    },
    async fetchMeetings() { calls.push(['meetings.fetch']); return []; },
  });
  if (delegate) globals.stopRecordingAttempt = delegate;
  runInNewContext(delegate ? delegationCode : controlCode, globals, {
    filename: 'recordingControls.extracted.cjs', timeout: 1000,
  });
  return {
    api: globals.exports, state, refs, calls, timers,
    count: (name) => calls.filter(([kind]) => kind === name).length,
    pipe(drain = Promise.resolve()) {
      const pipe = {
        meetingId: recordingId, chunks: [], bufferedChars: 0, closed: false,
        cleanup() { calls.push(['pipe.cleanup']); },
        session: { stop() { calls.push(['session.stop']); return drain; } },
      };
      refs.azurePipeRef.current = pipe;
      return pipe;
    },
  };
}

function assertNoStop(h) {
  assert.equal(h.count('native.stop'), 0);
  assert.equal(h.count('pipe.cleanup'), 0);
  assert.equal(h.count('session.stop'), 0);
  assert.equal(h.count('meetings.fetch'), 0);
}

test('both real RecordButton call sites discard MouseEvent and delegate the CURRENT attempt only', async () => {
  const received = [];
  const completion = Promise.resolve();
  const h = harness({
    status: 'recording', attempt: attemptFor('old'),
    delegate: (...args) => { received.push(args); return completion; },
  });
  const buttons = h.api.recordButtons();
  assert.deepEqual(Array.from(buttons, (button) => button.props.className).sort(), ['mini-record', 'primary-stop']);
  for (const nextAttempt of [attemptFor('live'), null]) {
    h.refs.recordingAttemptRef.current = nextAttempt;
    for (const element of buttons) {
      assert.equal(element.type, h.api.RecordButton);
      assert.equal(element.props.onStop, h.api.handleStopRecording, 'Use actual App JSX wiring');
      const button = element.type(element.props);
      assert.equal(button.type, 'button');
      assert.equal(button.props.disabled, false);
      const event = new MouseEvent();
      assert.equal(button.props.onClick(event), completion, 'Preserve delegated promise');
      assert.deepEqual(received.at(-1), [nextAttempt], 'Never forward the click event or captured old attempt');
    }
  }
  assert.equal(received.length, 4);
  await completion;
});

test('real button click runs the real stop path and coalesces simultaneous Stop requests', async () => {
  const query = deferred();
  const attempt = attemptFor('live');
  const h = harness({ status: 'recording', attempt, recordingId: 'live', runtime: () => query.promise });
  const pipe = h.pipe();
  h.api.startTimers();
  const element = h.api.recordButtons()[0];
  const stopped = element.type(element.props).props.onClick(new MouseEvent());
  assert.equal(h.api.handleStopRecording(new MouseEvent()), stopped);
  await flush();
  assert.equal(h.state.status, 'saving');
  assert.equal(h.count('runtime'), 1);
  query.resolve(active());
  await stopped;
  await h.refs.recordingQueueRef.current;
  assert.equal(h.count('native.stop'), 1);
  assert.equal(h.count('pipe.cleanup'), 1);
  assert.equal(h.count('session.stop'), 1);
  assert.equal(h.count('meetings.fetch'), 1);
  assert.ok(h.calls.findIndex(([name]) => name === 'native.stop') < h.calls.findIndex(([name]) => name === 'pipe.cleanup'));
  assert.equal(pipe.closed, true);
  assert.equal(h.refs.recordingMeetingIdRef.current, null);
  assert.equal(h.refs.azurePipeRef.current, null);
  assert.equal(h.refs.stopRecordingPromiseRef.current, null);
  assert.equal(h.refs.recordingOperationsRef.current, 0);
  assert.equal(h.state.recordingOperationBusy, false);
  assert.equal(h.state.status, 'idle');
  assert.equal(h.state.error, null);
  assert.equal(h.timers.size, 0);
});

for (const sharedStop of [false, true]) {
  test(`stopRecordingAttempt rejects foreign identities BEFORE queue/shared-stop checks (shared=${sharedStop})`, async () => {
    const attempt = attemptFor('live');
    const h = harness({ status: 'recording', attempt, recordingId: 'live' });
    const pending = sharedStop ? Promise.resolve() : null;
    h.refs.stopRecordingPromiseRef.current = pending;
    const queue = h.refs.recordingQueueRef.current;
    for (const foreign of [new MouseEvent(), attemptFor('live'), null]) {
      const result = h.api.stopRecordingAttempt(foreign);
      assert.notEqual(result, pending, 'A stale request must not join the current stop');
      await result;
    }
    assert.deepEqual(h.calls, []);
    assert.equal(h.refs.recordingQueueRef.current, queue);
    assert.equal(h.refs.recordingOperationsRef.current, 0);
    assert.equal(h.refs.stopRecordingPromiseRef.current, pending);
    assert.equal(h.refs.recordingMeetingIdRef.current, 'live');
  });
}

test('stop queued behind initialization rechecks attempt identity before touching a newer recording', async () => {
  const gate = deferred();
  const old = attemptFor('old');
  const h = harness({ attempt: old, recordingId: 'old' });
  const init = h.api.enqueueRecordingOperation(() => gate.promise);
  const stop = h.api.stopRecordingAttempt(old);
  const newer = h.api.beginRecordingAttempt();
  h.api.identifyRecordingAttempt(newer, 'new');
  h.refs.recordingMeetingIdRef.current = 'new';
  gate.resolve();
  await Promise.all([init, stop]);
  await h.refs.recordingQueueRef.current;
  assert.equal(h.count('runtime'), 0);
  assert.equal(h.count('status'), 0);
  assertNoStop(h);
  assert.equal(h.refs.recordingMeetingIdRef.current, 'new');
  assert.equal(h.refs.recordingAttemptRef.current, newer);
  assert.equal(h.refs.recordingOperationsRef.current, 0);
});

for (const initializing of [false, true]) {
  test(`idle/native false selects without a stop modal, including initialization work (queued=${initializing})`, async () => {
    const h = harness();
    const gate = deferred();
    const init = initializing ? h.api.enqueueRecordingOperation(() => gate.promise) : Promise.resolve();
    const queue = h.refs.recordingQueueRef.current;
    try {
      assert.equal(h.refs.recordingOperationsRef.current, initializing ? 1 : 0);
      await h.api.handleSelectMeeting('target');
      assert.equal(h.count('runtime'), 0, 'Idle local navigation must not wait on native locks');
      assert.equal(h.state.activeMeetingId, 'target');
      assert.equal(h.state.pendingMeetingId, null);
      assert.equal(h.state.status, 'idle');
      assert.equal(h.state.error, null);
      assert.equal(h.state.fastUploadConsent, false);
      assert.equal(h.refs.fastConsentRef.current, null);
      assert.equal(h.refs.recordingQueueRef.current, queue);
      assert.equal(h.refs.recordingOperationsRef.current, initializing ? 1 : 0);
      assertNoStop(h);
    } finally {
      gate.resolve();
      await init;
      await h.refs.recordingQueueRef.current;
    }
  });
}

for (const target of ['selected', 'other']) {
  test(`idle selection reveals ${target} meeting without runtime IPC`, async () => {
    const h = harness({ runtime: () => { throw new Error('Idle browsing must not invoke runtime'); } });
    h.state.showSettings = true;
    h.state.showBatchTranscription = true;
    h.state.showVideoWorkspace = true;
    await h.api.handleSelectMeeting(target);
    assert.equal(h.state.activeMeetingId, target);
    assert.equal(h.state.showSettings, false);
    assert.equal(h.state.showBatchTranscription, false);
    assert.equal(h.state.showVideoWorkspace, false);
    assert.equal(h.count('runtime'), 0);
    assertNoStop(h);
  });
}

for (const existingAttempt of [false, true]) {
  test(`native true prompts instead of selecting or stopping (existing attempt=${existingAttempt})`, async () => {
    const attempt = existingAttempt ? attemptFor('live') : null;
    const h = harness({ status: 'recording', attempt, runtime: () => active() });
    await h.api.handleSelectMeeting('target');
    assert.equal(h.state.pendingMeetingId, 'target');
    assert.equal(h.state.activeMeetingId, 'selected');
    assert.equal(h.state.status, 'recording');
    assert.equal(h.state.error, null);
    assert.equal(h.refs.recordingMeetingIdRef.current, 'live');
    assert.equal(h.refs.recordingAttemptRef.current.meetingId, 'live');
    if (existingAttempt) assert.equal(h.refs.recordingAttemptRef.current, attempt);
    assert.equal(h.timers.size, 1);
    assertNoStop(h);
  });
}

test('native false with stale ref uses the real serialized stop and awaits Azure drain before selecting', async () => {
  const gate = deferred();
  const drain = deferred();
  const h = harness({ attempt: attemptFor('stale'), recordingId: 'stale' });
  const pipe = h.pipe(drain.promise);
  const init = h.api.enqueueRecordingOperation(() => gate.promise);
  const selected = h.api.handleSelectMeeting('target');
  try {
    await flush();
    assert.equal(h.count('runtime'), 1, 'Selection checks native before enqueueing cleanup');
    assert.equal(h.refs.recordingOperationsRef.current, 2, 'Cleanup is queued behind initialization');
    assert.ok(h.refs.stopRecordingPromiseRef.current);
    assert.equal(h.refs.recordingMeetingIdRef.current, 'stale');
    assert.equal(pipe.closed, false);
    assert.equal(h.state.activeMeetingId, 'selected');
    assertNoStop(h);
    gate.resolve();
    await init;
    await flush();
    assert.equal(h.count('runtime'), 2, 'Queued stop rechecks native runtime');
    assert.equal(h.count('native.stop'), 0, 'Never stop native when already inactive');
    assert.equal(h.count('pipe.cleanup'), 1);
    assert.equal(h.count('session.stop'), 1);
    assert.equal(pipe.closed, true);
    assert.equal(h.refs.recordingMeetingIdRef.current, null);
    assert.equal(h.refs.azurePipeRef.current, null);
    assert.equal(h.state.status, 'saving');
    assert.equal(h.state.activeMeetingId, 'selected', 'Do not select before drain finishes');
    assert.equal(h.count('meetings.fetch'), 0);
    drain.resolve();
    await selected;
    await h.refs.recordingQueueRef.current;
    assert.equal(h.state.activeMeetingId, 'target');
    assert.equal(h.state.pendingMeetingId, null);
    assert.equal(h.state.status, 'idle');
    assert.equal(h.state.error, null);
    assert.equal(h.count('meetings.fetch'), 1);
    assert.equal(h.refs.recordingOperationsRef.current, 0);
    assert.equal(h.state.recordingOperationBusy, false);
    assert.equal(h.refs.stopRecordingPromiseRef.current, null);
    assert.equal(h.calls.some(([name, value]) => name === 'pendingMeetingId' && value !== null), false);
  } finally {
    gate.resolve();
    drain.resolve();
    await selected;
    await h.refs.recordingQueueRef.current;
  }
});

test('selection runtime query failure reports the error without detaching the live ref or pipe', async () => {
  const attempt = attemptFor('live');
  const h = harness({
    status: 'recording', attempt, recordingId: 'live',
    runtime: () => { throw new Error('Synthetic runtime query failure'); },
  });
  const pipe = h.pipe();
  h.api.startTimers();
  const queue = h.refs.recordingQueueRef.current;
  await h.api.handleSelectMeeting('target');
  assert.equal(h.state.error, 'Synthetic runtime query failure');
  assert.equal(h.state.activeMeetingId, 'selected');
  assert.equal(h.state.pendingMeetingId, null);
  assert.equal(h.state.status, 'recording');
  assert.equal(h.refs.recordingAttemptRef.current, attempt);
  assert.equal(h.refs.recordingMeetingIdRef.current, 'live');
  assert.equal(h.refs.azurePipeRef.current, pipe);
  assert.equal(pipe.closed, false);
  assert.equal(h.refs.recordingQueueRef.current, queue);
  assert.equal(h.timers.size, 1);
  assertNoStop(h);
});

test('stop runtime query and confirmation failures preserve live capture and report retry guidance', async () => {
  const h = harness({
    status: 'recording', attempt: attemptFor('live'), recordingId: 'live',
    runtime: () => { throw new Error('Synthetic stop query failure'); },
  });
  const pipe = h.pipe();
  h.api.startTimers();
  await h.api.handleStopRecording();
  await h.refs.recordingQueueRef.current;
  assert.equal(h.count('runtime'), 2);
  assert.equal(h.count('native.stop'), 0);
  assert.equal(h.count('pipe.cleanup'), 0);
  assert.equal(h.count('session.stop'), 0);
  assert.equal(h.refs.recordingMeetingIdRef.current, 'live');
  assert.equal(h.refs.azurePipeRef.current, pipe);
  assert.equal(pipe.closed, false);
  assert.equal(h.state.status, 'recording');
  assert.equal(h.state.error, 'Synthetic stop query failure Could not confirm capture stopped; retry Stop.');
  assert.equal(h.timers.size, 1, 'Live timer is restored after failed stop');
  assert.equal(h.refs.stopRecordingPromiseRef.current, null);
  assert.equal(h.refs.recordingOperationsRef.current, 0);
});

for (const nativeActive of [false, true]) {
  for (const invalidation of ['new operation', 'new attempt', 'cancel selection']) {
    test(`in-flight selection is discarded after ${invalidation} (native active=${nativeActive})`, async () => {
      const query = deferred();
      const gate = deferred();
      const h = harness({ status: 'recording', runtime: () => query.promise });
      const selected = h.api.handleSelectMeeting('obsolete-target');
      let operation = Promise.resolve();
      if (invalidation === 'new operation') {
        operation = h.api.enqueueRecordingOperation(() => gate.promise);
        assert.equal(h.refs.recordingOperationsRef.current, 1);
      } else if (invalidation === 'new attempt') {
        h.api.beginRecordingAttempt();
      } else {
        h.api.handleCancelSwitch();
      }
      const currentAttempt = h.refs.recordingAttemptRef.current;
      const beforeResult = h.calls.length;
      try {
        query.resolve(nativeActive ? active('obsolete-native') : inactive());
        await selected;
        assert.deepEqual(h.calls.slice(beforeResult), [], 'Stale result must not mutate state or start cleanup');
        assert.equal(h.state.activeMeetingId, 'selected');
        assert.equal(h.state.pendingMeetingId, null);
        assert.equal(h.state.error, null);
        assert.equal(h.refs.recordingMeetingIdRef.current, null);
        assert.equal(h.refs.recordingAttemptRef.current, currentAttempt);
        assertNoStop(h);
      } finally {
        gate.resolve();
        await operation;
        await h.refs.recordingQueueRef.current;
      }
    });
  }
}
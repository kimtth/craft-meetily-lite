import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import { runInNewContext } from 'node:vm';
import ts from 'typescript';

// Run with node --test src/audio/azureSession.test.mjs. Like timing.test.mjs,
// transpile the actual TS in memory. CommonJS + a VM require allowlist mocks
// both static and dynamic imports without experimental VM-module flags, an
// SDK installation at runtime, cloud access, or changes to the production API.
async function compile(relativePath) {
  const source = await readFile(new URL(relativePath, import.meta.url), 'utf8');
  return ts.transpileModule(source, {
    compilerOptions: { target: ts.ScriptTarget.ES2020, module: ts.ModuleKind.CommonJS },
  }).outputText;
}

const [sessionCode, timelineCode, languageCode] = await Promise.all([
  compile('../azureSpeech.ts'), compile('./pcmTimeline.ts'), compile('./recordingOptions.ts'),
]);

function evaluate(code, dependencies = {}, globals = {}) {
  const exports = {};
  runInNewContext(code, {
    exports, URL, Error,
    require(name) {
      assert.ok(Object.hasOwn(dependencies, name), `Unexpected/unmocked import: ${name}`);
      return dependencies[name];
    },
    ...globals,
  });
  return exports;
}

const flush = () => new Promise((resolve) => setImmediate(resolve));
function deferred() {
  let resolve;
  let reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

function harness(behavior = {}) {
  const calls = [];
  const errors = [];
  const writes = [];
  const timers = new Set();
  let recognizer;
  let config;
  const pushStream = {
    write(bytes) { writes.push(Buffer.from(bytes)); calls.push('write'); },
    close() { calls.push('input.close'); behavior.onInputClose?.(recognizer); },
  };
  const sdk = {
    ResultReason: { RecognizedSpeech: 1 },
    CancellationReason: { Error: 0, EndOfStream: 1, CanceledByUser: 2 },
    PropertyId: { Speech_SegmentationStrategy: 'segmentation' },
    SpeechConfig: {
      fromEndpoint() { config = { setProperty() {} }; return config; },
    },
    AudioStreamFormat: {
      getWaveFormatPCM(...format) { assert.deepEqual(format, [16000, 16, 1]); return {}; },
    },
    AudioInputStream: { createPushStream() { return pushStream; } },
    AudioConfig: {
      fromStreamInput(input) {
        assert.equal(input, pushStream);
        return { close() { calls.push('audio.close'); behavior.onAudioClose?.(); } };
      },
    },
    SpeechRecognizer: class {
      constructor() { recognizer = this; }
      startContinuousRecognitionAsync(resolve, reject) {
        calls.push('start');
        if (behavior.onStart) behavior.onStart(this, resolve, reject);
        else resolve();
      }
      stopContinuousRecognitionAsync(resolve) {
        // Keep this functional so the old implementation fails assertions,
        // not merely because the mock is missing its premature-stop API.
        calls.push('forced.stop');
        resolve?.();
      }
      close(resolve, reject) {
        calls.push('recognizer.close');
        if (behavior.onDispose) behavior.onDispose(this, resolve, reject);
        else resolve?.();
      }
    },
  };
  const { startAzureSpeechSession } = evaluate(sessionCode, {
    './nativeClient': {
      getAzureCliAccessToken() { throw new Error('A lifecycle test must never request a real token'); },
    },
    './audio/pcmTimeline': evaluate(timelineCode),
    './audio/recordingOptions': evaluate(languageCode),
    'microsoft-cognitiveservices-speech-sdk': sdk,
  }, {
    window: { atob: (value) => Buffer.from(value, 'base64').toString('binary') },
    setTimeout(callback, milliseconds) {
      const timer = { callback, milliseconds };
      timers.add(timer);
      return timer;
    },
    clearTimeout(timer) { timers.delete(timer); },
  });

  return {
    calls, errors, writes, timers,
    get recognizer() { return recognizer; },
    get config() { return config; },
    start(overrides = {}) {
      return startAzureSpeechSession({
        endpoint: 'https://test-resource.cognitiveservices.azure.com/',
        language: 'en', onFinalText() {}, onError: (message) => errors.push(message),
        ...overrides,
      });
    },
    final(text, offset = 0) {
      recognizer.recognized(recognizer, { result: { reason: 1, text, offset } });
    },
    end() { recognizer.sessionStopped(recognizer, {}); },
    cancel(reason = sdk.CancellationReason.Error, errorDetails = '') {
      recognizer.canceled(recognizer, { reason, errorDetails });
    },
    expire(milliseconds) {
      const matching = [...timers].filter((timer) => timer.milliseconds === milliseconds);
      assert.equal(matching.length, 1, `Expected one active ${milliseconds}ms deadline`);
      timers.delete(matching[0]);
      matching[0].callback();
    },
    assertDisposed() {
      assert.equal(calls.filter((call) => call === 'input.close').length, 1);
      assert.equal(calls.filter((call) => call === 'recognizer.close').length, 1);
      assert.equal(calls.filter((call) => call === 'audio.close').length, 1);
      assert.ok(!calls.includes('forced.stop'));
      assert.equal(timers.size, 0);
    },
  };
}

test('stop drains late backlog finals and waits for every persistence before disposal', async () => {
  const h = harness();
  const early = deferred();
  const tail = deferred();
  const saved = [];
  const session = await h.start({
    onFinalText: (text, offset) => {
      saved.push([text, offset]);
      return text === 'early' ? early.promise : tail.promise;
    },
  });
  session.pushPcmBase64(Buffer.alloc(32_000).toString('base64'), 40);
  session.pushPcmBase64(Buffer.alloc(32_000).toString('base64'), 41);
  h.final(' early ', 5_000_000);
  await flush();
  const stopped = session.stop();
  assert.equal(session.stop(), stopped, 'stop must return the same in-flight promise');
  session.pushPcmBase64('ignored after stop', 999);
  await flush();
  assert.equal(h.writes.length, 2);
  assert.ok(!h.calls.includes('recognizer.close'));
  assert.ok(!h.calls.includes('forced.stop'));

  h.final('tail', 15_000_000);
  h.cancel(1); // Installed SDK emits EndOfStream before sessionStopped.
  await flush();
  assert.ok(!h.calls.includes('recognizer.close'), 'EOF cancellation alone is not completion');
  h.end();
  await flush();
  assert.deepEqual(saved, [['early', 40.5], ['tail', 41.5]]);
  assert.ok(!h.calls.includes('recognizer.close'));
  tail.resolve();
  await flush();
  assert.ok(!h.calls.includes('recognizer.close'), 'an earlier final is still persisting');
  early.resolve();
  await stopped;
  assert.equal(session.stop(), stopped);
  h.final('must not persist after disposal');
  assert.equal(saved.length, 2);
  assert.deepEqual(h.errors, []);
  h.assertDisposed();
});

test('synchronous EOF/sessionStopped during input close is race-safe and idempotent', async () => {
  const h = harness({
    onInputClose(recognizer) {
      recognizer.canceled(recognizer, { reason: 1 });
      recognizer.sessionStopped(recognizer, {});
    },
  });
  const session = await h.start();
  const first = session.stop();
  assert.equal(first, session.stop());
  await first;
  h.assertDisposed();
});

test('stop waits for the asynchronous SDK close callback after persistence', async () => {
  let completeClose;
  const h = harness({ onDispose: (_, resolve) => { completeClose = resolve; } });
  const session = await h.start();
  const stopped = session.stop();
  let resolved = false;
  void stopped.then(() => { resolved = true; });
  await flush();
  h.end();
  await flush();
  assert.equal(typeof completeClose, 'function');
  assert.equal(resolved, false);
  assert.ok(!h.calls.includes('audio.close'));
  completeClose();
  await stopped;
  h.assertDisposed();
});

test('language replacement preserves the old session callbacks and timeline until drain', async () => {
  const old = harness();
  const next = harness();
  const oldTexts = [];
  const nextTexts = [];
  const options = { language: 'EN_us', onFinalText: (text, offset) => oldTexts.push([text, offset]) };
  const oldSession = await old.start(options);
  assert.equal(old.config.speechRecognitionLanguage, 'en-US');
  oldSession.pushPcmBase64(Buffer.alloc(32_000).toString('base64'), 10);
  options.language = 'ko';
  options.onFinalText = (text, offset) => nextTexts.push([text, offset]);
  const stopped = oldSession.stop();
  await flush();
  old.final('old backlog', 5_000_000);
  old.end();
  await stopped;
  const nextSession = await next.start(options);
  nextSession.pushPcmBase64(Buffer.alloc(32_000).toString('base64'), 11);
  next.final('new language', 5_000_000);
  const nextStopped = nextSession.stop();
  await flush();
  next.end();
  await nextStopped;
  assert.equal(next.config.speechRecognitionLanguage, 'ko-KR');
  assert.deepEqual(oldTexts, [['old backlog', 10.5]]);
  assert.deepEqual(nextTexts, [['new language', 11.5]]);
  old.assertDisposed();
  next.assertDisposed();
});

test('missing natural sessionStopped times out as incomplete, then disposes', async () => {
  const h = harness();
  const session = await h.start();
  const stopped = session.stop();
  const rejected = assert.rejects(stopped, /incomplete.*Timed out.*recognition/i);
  await flush();
  h.cancel(1);
  await flush();
  assert.ok(!h.calls.includes('recognizer.close'));
  h.expire(120_000);
  await rejected;
  assert.equal(h.errors.length, 1);
  assert.equal(stopped, session.stop());
  h.assertDisposed();
});

test('hung final persistence is bounded and never reported as successful drain', async () => {
  const h = harness();
  const persist = deferred();
  const session = await h.start({ onFinalText: () => persist.promise });
  h.final('final');
  const stopped = session.stop();
  const rejected = assert.rejects(stopped, /incomplete.*Timed out.*persistence/i);
  await flush();
  h.end();
  await flush();
  assert.ok(!h.calls.includes('recognizer.close'));
  h.expire(120_000);
  await rejected;
  persist.resolve();
  await flush();
  h.assertDisposed();
});

test('a final persistence rejection remains a stop failure after leaving the pending set', async () => {
  const h = harness();
  const session = await h.start({ onFinalText: async () => { throw new Error('store unavailable'); } });
  h.final('cannot save');
  await flush();
  const stopped = session.stop();
  const rejected = assert.rejects(stopped, /incomplete.*store unavailable/);
  await flush();
  h.end();
  await rejected;
  assert.equal(h.errors.length, 1);
  h.assertDisposed();
});

test('cancellation without sessionStopped cleans up but awaits existing final persistence', async () => {
  const h = harness();
  const persist = deferred();
  const session = await h.start({ onFinalText: () => persist.promise });
  h.final('before cancellation');
  await flush();
  h.cancel(0, 'network lost');
  await flush();
  assert.ok(h.calls.includes('input.close'));
  assert.ok(!h.calls.includes('recognizer.close'));
  const rejected = assert.rejects(session.stop(), /incomplete.*network lost/);
  persist.resolve();
  await rejected;
  assert.equal(h.errors.length, 1);
  h.assertDisposed();
});

test('cancellation during drain rejects the shared stop and does not need sessionStopped', async () => {
  const h = harness();
  const session = await h.start();
  const stopped = session.stop();
  const rejected = assert.rejects(stopped, /incomplete.*service failure/);
  await flush();
  h.cancel(0, 'service failure');
  await rejected;
  assert.equal(stopped, session.stop());
  h.assertDisposed();
});

for (const signal of ['unexpected sessionStopped', 'unexpected EOF', 'user cancellation']) {
  test(`${signal} before input close is an incomplete session and triggers cleanup`, async () => {
    const h = harness();
    const session = await h.start();
    if (signal === 'unexpected sessionStopped') h.end();
    else h.cancel(signal === 'unexpected EOF' ? 1 : 2);
    await flush();
    await assert.rejects(session.stop(), /incomplete/);
    h.assertDisposed();
  });
}

for (const mode of ['error callback', 'synchronous throw', 'canceled then success', 'canceled without callback']) {
  test(`start failure (${mode}) rejects and cleans up without needing caller stop`, async () => {
    const h = harness({
      onStart(recognizer, resolve, reject) {
        if (mode === 'error callback') reject('start failed');
        else if (mode === 'synchronous throw') throw new Error('start failed');
        else {
          recognizer.canceled(recognizer, { reason: 0, errorDetails: 'start failed' });
          if (mode === 'canceled then success') resolve();
        }
      },
    });
    await assert.rejects(h.start(), /incomplete.*start failed/);
    assert.equal(h.errors.length, 1);
    h.assertDisposed();
  });
}

test('a stalled start callback is bounded and resources are closed', async () => {
  const h = harness({ onStart() {} });
  const rejected = assert.rejects(h.start(), /incomplete.*Timed out starting/);
  await flush();
  h.expire(30_000);
  await rejected;
  h.assertDisposed();
});

test('throwing onError cannot interrupt cancellation cleanup', async () => {
  const h = harness();
  const session = await h.start({ onError() { throw new Error('consumer callback failed'); } });
  assert.doesNotThrow(() => h.cancel(0, 'original service error'));
  await assert.rejects(session.stop(), /incomplete.*original service error/);
  h.assertDisposed();
});

test('input close failure rejects as incomplete and still disposes', async () => {
  const h = harness({ onInputClose() { throw new Error('input close failed'); } });
  const session = await h.start();
  await assert.rejects(session.stop(), /incomplete.*input close failed/);
  h.assertDisposed();
});

test('disposal callback failure still closes audio config and rejects stop', async () => {
  const h = harness({ onDispose: (_, __, reject) => reject('dispose failed') });
  const session = await h.start();
  const stopped = session.stop();
  const rejected = assert.rejects(stopped, /incomplete.*dispose failed/);
  await flush();
  h.end();
  await rejected;
  h.assertDisposed();
});

test('stalled disposal is bounded and still releases audio config', async () => {
  const h = harness({ onDispose() {} });
  const session = await h.start();
  const stopped = session.stop();
  const rejected = assert.rejects(stopped, /incomplete.*Timed out disposing/);
  await flush();
  h.end();
  await flush();
  h.expire(5_000);
  await rejected;
  h.assertDisposed();
});
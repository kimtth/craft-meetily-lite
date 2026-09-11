/**
 * Captured-native-wire browser E2E. No native process, credentials or cloud.
 * node scripts/recording-e2e.mjs --wire <capture.ndjson> [--playwright <module-dir>]
 *   [--chromium <executable>] [--out <directory>] [--speed 1]
 * node scripts/recording-e2e.mjs --navigation-only [--out <directory>]
 * node scripts/recording-e2e.mjs --synthetic-wire --speed 0 [--out <directory>]
 * Navigation-only implies synthetic PCM; no captured wire is required. The
 * default suite includes navigation regressions AND the original stop tests.
 * Only --server (private child mode) installs test resolvers; production Vite
 * configuration, App, main, nativeClient, and azureSpeech remain unchanged.
 */
import assert from 'node:assert/strict';
import { fork } from 'node:child_process';
import { createHash } from 'node:crypto';
import { createRequire } from 'node:module';
import { createServer as createNetServer } from 'node:net';
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { navigationScenarios, runNavigationScenario } from './recording-e2e/navigation.mjs';
import { syntheticWire } from './recording-e2e/synthetic-wire.mjs';

const here = fileURLToPath(import.meta.url);
const root = path.resolve(path.dirname(here), '..');
const args = process.argv.slice(2);
const option = (name, fallback) => args.includes(name) ? args[args.indexOf(name) + 1] : fallback;
const hash = (bytes) => createHash('sha256').update(bytes).digest('hex');

async function serve() {
  const { createServer } = await import('vite');
  const { default: react } = await import('@vitejs/plugin-react');
  const require = createRequire(import.meta.url);
  const wire = process.env.MEETLY_E2E_SYNTHETIC === '1'
    ? syntheticWire() : await readFile(process.env.MEETLY_E2E_WIRE);
  const cacheDir = await mkdtemp(path.join(tmpdir(), 'meetly-e2e-vite-'));
  const port = await new Promise((resolve, reject) => {
    const socket = createNetServer();
    socket.once('error', reject);
    socket.listen(0, '127.0.0.1', () => {
      const port = socket.address().port;
      socket.close(() => resolve(port));
    });
  });
  let server;
  const cleanup = async () => {
    await server?.close();
    await rm(cacheDir, { recursive: true, force: true });
    process.exit(0);
  };
  process.on('message', (message) => { if (message === 'stop') void cleanup(); });
  process.on('disconnect', () => { void cleanup(); });
  server = await createServer({
    root, configFile: false, cacheDir, clearScreen: false, logLevel: 'warn',
    resolve: { alias: { 'recording-e2e-real-sdk': require.resolve('microsoft-cognitiveservices-speech-sdk') } },
    optimizeDeps: { include: ['recording-e2e-real-sdk'] },
    plugins: [react(), {
      name: 'offline-recording-e2e-only', enforce: 'pre',
      resolveId(id) {
        if (id === 'microsoft-cognitiveservices-speech-sdk') return path.join(root, 'scripts/recording-e2e/speech-sdk.mjs');
      },
      transformIndexHtml(html) {
        return html.replace('src="/src/main.tsx"', 'src="/scripts/recording-e2e/bootstrap.mjs"');
      },
      configureServer(vite) {
        vite.middlewares.use((request, response, next) => {
          response.setHeader('Cache-Control', 'no-store');
          response.setHeader('Content-Security-Policy', "default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; font-src 'self'; connect-src 'self'; media-src 'none'; object-src 'none'; frame-src 'none'; base-uri 'none'; form-action 'none'");
          if (request.url === '/__recording-e2e/wire') {
            response.setHeader('Content-Type', 'application/x-ndjson');
            response.end(wire);
          } else next();
        });
      },
    }],
    server: { host: '127.0.0.1', port, strictPort: true, fs: { allow: [root, cacheDir] } },
  });
  await server.listen();
  process.send({ origin: `http://127.0.0.1:${server.httpServer.address().port}` });
}

async function run() {
  const navigationOnly = args.includes('--navigation-only');
  const synthetic = navigationOnly || args.includes('--synthetic-wire');
  assert.ok(!(synthetic && args.includes('--wire')), '--wire cannot be combined with synthetic/navigation-only mode');
  const wirePath = synthetic ? null : path.resolve(option('--wire', process.env.MEETLY_E2E_WIRE || ''));
  assert.ok(synthetic || args.includes('--wire') || process.env.MEETLY_E2E_WIRE, '--wire, --synthetic-wire or --navigation-only is required');
  const wire = synthetic ? syntheticWire() : await readFile(wirePath);
  const records = wire.toString('utf8').trim().split(/\r?\n/).map(JSON.parse);
  const pcm = Buffer.concat(records.map((record, index) => {
    assert.deepEqual(Object.keys(record).sort(), ['meetingId', 'offsetSeconds', 'pcmBase64']);
    const bytes = Buffer.from(record.pcmBase64, 'base64');
    assert.equal(bytes.length % 2, 0);
    assert.equal(bytes.toString('base64'), record.pcmBase64);
    if (index) {
      const previous = records[index - 1];
      assert.ok(Math.abs(record.offsetSeconds - previous.offsetSeconds - Buffer.from(previous.pcmBase64, 'base64').length / 32000) < 1e-7, 'Wire PCM timeline must be contiguous');
    }
    return bytes;
  }));
  assert.equal(records.length, 142);
  assert.equal(pcm.length / 2, 570767);
  assert.equal(Buffer.from(records.at(-1).pcmBase64, 'base64').length / 2, 10400);
  assert.equal(records[0].offsetSeconds, 0);
  const out = path.resolve(option('--out', path.join(root, 'artifacts/recording-e2e')));
  await mkdir(out, { recursive: true });
  const speed = Number(option('--speed', '1'));
  assert.ok(Number.isFinite(speed) && speed >= 0);
  const playwrightPath = option('--playwright', process.env.MEETLY_PLAYWRIGHT || 'C:/Users/taehokim/AppData/Local/npm-cache/_npx/e41f203b7505f1fb/node_modules/playwright');
  const { chromium } = await import(pathToFileURL(path.join(playwrightPath, 'index.mjs')).href);
  const child = fork(here, ['--server'], {
    cwd: root, env: { ...process.env, MEETLY_E2E_WIRE: wirePath || '', MEETLY_E2E_SYNTHETIC: synthetic ? '1' : '0' }, stdio: ['ignore', 'pipe', 'pipe', 'ipc'],
  });
  let serverLog = '';
  child.stdout.on('data', (data) => { serverLog += data; });
  child.stderr.on('data', (data) => { serverLog += data; });
  let browser;
  const report = {
    status: 'running', boundary: synthetic
      ? 'Full real mounted App; entirely synthetic fixtures, no native app/hardware/cloud startup'
      : 'Full real mounted App; previously captured native wire replay, NOT simultaneous hardware/Tauri-webview/cloud E2E',
    mocks: ['allowlisted native IPC and synthetic localStorage persistence', 'SpeechRecognizer/service only; synthetic partial/final text'],
    real: ['React StrictMode main/App', 'Tauri JS core/event/listen/emit and official mockIPC', 'azureSpeech.ts and PcmTimeline', 'installed Speech SDK SpeechConfig, AudioStreamFormat, AudioInputStream, AudioConfig, reader buffering and EOF'],
    consent: navigationOnly ? 'No recording or recognition is started.' : 'Live append dialog discloses Azure upload; explicit Append click is exercised. No live consent checkbox exists. No cloud upload occurs.',
    wire: { source: synthetic ? 'generated PCM silence' : 'captured native NDJSON', path: wirePath, bytes: wire.length, sha256: hash(wire), records: records.length, pcmBytes: pcm.length, pcmSha256: hash(pcm), samples: pcm.length / 2, durationSeconds: pcm.length / 32000, finalTailSamples: 10400 },
    scenarios: [],
  };
  try {
    const { origin } = await new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error(`Vite startup timed out: ${serverLog}`)), 60000);
      child.once('message', (message) => { clearTimeout(timer); resolve(message); });
      child.once('exit', (code) => { clearTimeout(timer); reject(new Error(`Vite exited ${code}: ${serverLog}`)); });
    });
    browser = await chromium.launch({
      headless: true,
      ...(option('--chromium') ? { executablePath: option('--chromium') } : {}),
      args: ['--disable-background-networking', '--disable-component-update', '--disable-domain-reliability', '--no-pings', '--host-resolver-rules=MAP * ~NOTFOUND, EXCLUDE 127.0.0.1'],
    });
    for (const scenario of [...(navigationOnly ? [] : ['stop-button', 'keep-and-stop-switch']), ...navigationScenarios]) {
      const context = await browser.newContext({ viewport: { width: 1440, height: 1080 }, serviceWorkers: 'block', locale: 'en-US', colorScheme: 'light' });
      const blocked = [];
      const pageErrors = [];
      const consoleErrors = [];
      // Fail closed: only this child server can receive HTTP; no SDK service,
      // fonts, credential endpoints, external WebSockets or service workers.
      await context.route('**/*', (route) => {
        if (new URL(route.request().url()).origin === origin) return route.continue();
        blocked.push(route.request().url());
        return route.abort('blockedbyclient');
      });
      await context.routeWebSocket('**/*', (socket) => {
        // Only this local Vite client's HMR transport is allowed.
        if (new URL(socket.url()).host === new URL(origin).host) socket.connectToServer();
        else { blocked.push(socket.url()); socket.close(); }
      });
      const page = await context.newPage();
      page.setDefaultTimeout(20000);
      // Each run uses a fresh optimizer cache; the real SDK's first bundle can
      // exceed the UI-action deadline on a busy Windows development machine.
      page.setDefaultNavigationTimeout(90000);
      page.on('pageerror', (error) => pageErrors.push(String(error)));
      page.on('console', (message) => { if (message.type() === 'error') consoleErrors.push(message.text()); });
      const item = { name: scenario, assertions: [], blocked, pageErrors, consoleErrors, screenshots: [] };
      report.scenarios.push(item);
      const check = (name, fn) => { fn(); item.assertions.push(name); };
      const wait = (predicate) => page.waitForFunction(predicate);
      const snapshot = (suffix) => {
        const filename = `${scenario}-${suffix}.png`;
        item.screenshots.push(filename);
        return page.screenshot({ path: path.join(out, filename), fullPage: true });
      };
      const select = async (title) => {
        if (!await page.locator('.meeting-list').isVisible()) await page.getByTitle('Toggle recordings', { exact: true }).click();
        await page.getByRole('button', { name: new RegExp(`^${title}`) }).click();
      };
      const ready = () => page.locator('.recording-state strong').filter({ hasText: /^Ready$/ }).waitFor();
      try {
        await page.goto(origin);
        if (navigationScenarios.includes(scenario)) {
          await runNavigationScenario({ page, item, snapshot });
          item.status = 'passed';
          console.log(`PASS ${scenario}: ${item.assertions.length} checks`);
          continue;
        }
        await wait(() => window.__recordingE2E?.settingsWaiting);
        await ready();
        await select('Synthetic Wire Session A');
        await page.getByRole('textbox', { name: 'Meeting title' }).waitFor();
        check('Ready during blocked initial settings: selection succeeds, no false modal', () => {});
        assert.equal(await page.getByRole('alertdialog').count(), 0);
        await page.evaluate(() => window.__recordingE2E.releaseSettings());
        await wait(() => window.__recordingE2E.catalogWaiting);
        await select('Synthetic Wire Session B');
        assert.equal(await page.getByRole('textbox', { name: 'Meeting title' }).inputValue(), 'Synthetic Wire Session B');
        assert.equal(await page.getByRole('alertdialog').count(), 0);
        await select('Synthetic Wire Session A');
        check('Delayed Foundry catalog: Ready navigation succeeds without modal', () => {});
        await snapshot('ready-delayed-catalog');

        await page.getByRole('button', { name: 'Settings', exact: true }).click();
        // Nested select option text participates in these implicit labels.
        await page.getByLabel(/^Transcription Engine/).selectOption('azure');
        await page.getByLabel(/^Capture Mode/).selectOption('system');
        await page.getByLabel(/^Session Language/).selectOption('en-US');
        await page.getByLabel('Speech Custom Domain Endpoint', { exact: true }).fill('https://offline-fixture.cognitiveservices.azure.com/');
        await page.getByRole('button', { name: 'Settings', exact: true }).click();
        await page.locator('button.primary-stop').click();
        const choice = page.getByRole('dialog', { name: 'Start recording' });
        await choice.waitFor();
        assert.match(await choice.innerText(), /audio is sent to Azure/);
        assert.equal(await choice.getByRole('checkbox').count(), 0);
        await choice.getByRole('button', { name: 'Cancel', exact: true }).click();
        assert.equal(await page.evaluate(() => window.__recordingE2E.calls.filter((call) => call.command === 'start_recording').length), 0);
        await page.locator('button.primary-stop').click();
        await choice.getByRole('button', { name: 'Append to open session', exact: true }).click();
        await page.locator('.recording-state strong').filter({ hasText: /^Recording$/ }).waitFor();
        await wait(() => window.__recordingE2E.speech.length === 1);
        assert.equal(await page.locator('.error-banner').count(), 0);
        check('Actual settings/options + Azure disclosure, Cancel does not start, explicit Append starts', () => {});
        if (scenario === 'keep-and-stop-switch') {
          await select('Synthetic Wire Session B');
          await page.getByRole('alertdialog', { name: 'Stop current recording?' }).waitFor();
          await snapshot('active-modal');
          await page.getByRole('button', { name: 'Keep recording', exact: true }).click();
          assert.equal(await page.getByRole('textbox', { name: 'Meeting title' }).inputValue(), 'Synthetic Wire Session A');
          assert.equal(await page.evaluate(() => window.__recordingE2E.calls.filter((call) => call.command === 'stop_recording').length), 0);
          check('Active switch shows modal; Keep preserves recording and makes zero native stops', () => {});
        }
        const startReplay = Date.now();
        await page.evaluate((replaySpeed) => window.__recordingE2E.replay(replaySpeed), scenario === 'stop-button' ? speed : 0);
        item.replayWallSeconds = (Date.now() - startReplay) / 1000;
        assert.equal(await page.evaluate(() => window.__recordingE2E.delivered.length), 141);
        assert.equal(await page.evaluate(() => window.__recordingE2E.catalogResolved), false);
        await snapshot('recording');
        if (scenario === 'stop-button') {
          // DOM click on the real RecordButton: never invoke App handlers.
          await page.locator('button.primary-stop').click();
        } else {
          await select('Synthetic Wire Session B');
          await page.getByRole('button', { name: 'Stop & switch', exact: true }).click();
        }
        await wait(() => window.__recordingE2E.persistenceWaiting);
        await page.locator('.recording-state strong').filter({ hasText: /^Saving$/ }).waitFor();
        assert.equal(await page.evaluate(() => window.__recordingE2E.speech[0].closed), 0);
        assert.equal(await page.evaluate(() => window.__recordingE2E.stored()[0].transcript.length), 1);
        check('Stop waits for native final tail, real SDK EOF and held final persistence; no early disposal', () => {});
        await snapshot('saving-final-persistence');
        await page.evaluate(() => window.__recordingE2E.releasePersistence());
        await ready();
        await wait(() => window.__recordingE2E.speech[0].closed === 1);
        if (scenario === 'keep-and-stop-switch') {
          assert.equal(await page.getByRole('textbox', { name: 'Meeting title' }).inputValue(), 'Synthetic Wire Session B');
          assert.equal(await page.getByRole('alertdialog').count(), 0);
          await select('Synthetic Wire Session A');
        }
        await page.getByText('SYNTHETIC final tail — offline recognizer, not actual speech', { exact: true }).waitFor();
        await snapshot('final-transcript');
        const state = await page.evaluate(() => {
          const s = window.__recordingE2E;
          return { wireHash: s.wireHash, wireBytes: s.wireBytes, calls: s.calls, log: s.log, delivered: s.delivered, speech: s.speech, failures: s.failures, stored: s.stored() };
        });
        check('Fetched original NDJSON bytes match input SHA-256 and length', () => {
          assert.equal(state.wireHash, hash(wire)); assert.equal(state.wireBytes, wire.length);
        });
        check('All 142 Tauri audio-chunk callbacks retain exact base64 and offsets, only meeting ID mapped', () => {
          assert.deepEqual(state.delivered, records.map((record) => ({ ...record, meetingId: 'synthetic-wire-a' })));
        });
        const consumed = Buffer.concat(state.speech[0].reads.map((value) => Buffer.from(value, 'base64')));
        check('Real SDK reader drains 1141534 bytes / 570767 samples byte-for-byte including final 10400-sample tail', () => {
          assert.deepEqual(consumed, pcm);
          assert.deepEqual(consumed.subarray(-20800), Buffer.from(records.at(-1).pcmBase64, 'base64'));
          assert.equal(state.speech[0].bytes, pcm.length);
          assert.deepEqual(state.speech[0].format, { samplesPerSec: 16000, bitsPerSample: 16, channels: 1 });
        });
        check('Exactly one native start/stop, one recognizer disposal, zero forced stops or token/inference IPC', () => {
          for (const command of ['start_recording', 'stop_recording']) assert.equal(state.calls.filter((call) => call.command === command).length, 1);
          assert.equal(state.speech.length, 1); assert.equal(state.speech[0].closed, 1);
          assert.equal(state.speech[0].forcedStops, 0); assert.equal(state.speech[0].eof, true);
          assert.equal(state.speech[0].language, 'en-US');
          assert.ok(!state.calls.some((call) => /token|transcribe_fast|copilot|download|sign_in_azure_cli/.test(call.command)));
        });
        check('Synthetic final text and exact timeline offsets persist after EOF; other meeting remains untouched', () => {
          const transcript = state.stored[0].transcript;
          assert.equal(transcript.length, 2);
          assert.equal(transcript[0].offsetSeconds, 0.125);
          assert.ok(Math.abs(transcript[1].offsetSeconds - (pcm.length / 32000 - 0.1)) < 1e-7);
          assert.equal(state.stored[1].transcript.length, 0);
          assert.equal(state.stored[0].durationSeconds, pcm.length / 32000);
          const order = ['native.stop', 'native.tailDelivered', 'native.stopReturned', 'speech.eof', 'speech.sessionStopped', 'persistence.waiting', 'persistence.finalSaved', 'recognizer.close'];
          for (let i = 1; i < order.length; i++) assert.ok(state.log.indexOf(order[i]) > state.log.indexOf(order[i - 1]), `${order[i - 1]} precedes ${order[i]}`);
        });
        assert.equal(await page.locator('.error-banner').count(), 0);
        assert.doesNotMatch(await page.locator('body').innerText(), /30 seconds behind|transcription buffer is full|incomplete|Failed to stop|reading ['"]then/);
        assert.deepEqual(state.failures, []);
        assert.deepEqual(pageErrors, []);
        // CSP may log blocked Google Fonts; those are recorded, not hidden.
        const unexpectedConsole = consoleErrors.filter((text) => !/Content Security Policy|content security policy|Failed to load resource.*ERR_BLOCKED_BY_CLIENT/.test(text));
        assert.deepEqual(unexpectedConsole, []);
        check('No UI recording/cleanup error, uncaught error, or unknown IPC', () => {});
        await select('Synthetic Wire Session B');
        assert.equal(await page.getByRole('textbox', { name: 'Meeting title' }).inputValue(), 'Synthetic Wire Session B');
        assert.equal(await page.getByRole('alertdialog').count(), 0);
        check('Idle navigation succeeds after Stop with no warning', () => {});
        await page.evaluate(() => window.__recordingE2E.releaseCatalog());
        await page.reload();
        await wait(() => window.__recordingE2E?.settingsWaiting);
        await page.evaluate(() => { window.__recordingE2E.releaseSettings(); window.__recordingE2E.releaseCatalog(); });
        await wait(() => window.__recordingE2E.catalogWaiting);
        await select('Synthetic Wire Session A');
        await page.getByText('SYNTHETIC final tail — offline recognizer, not actual speech', { exact: true }).waitFor();
        assert.equal(await page.locator('.error-banner').count(), 0);
        check('Full page reload restores final transcript from isolated mock localStorage store', () => {});
        item.pcm = { sha256: hash(consumed), bytes: consumed.length, sdkReadChunks: state.speech[0].reads.length, deliveredRecords: state.delivered.length };
        item.transcript = state.stored[0].transcript;
        item.nativeCalls = state.calls;
        item.lifecycle = state.log;
        item.status = 'passed';
        console.log(`PASS ${scenario}: ${item.assertions.length} checks, ${consumed.length} PCM bytes, ${state.delivered.length} records`);
      } catch (error) {
        item.status = 'failed';
        item.error = String(error.stack || error);
        await snapshot('failure').catch(() => {});
        await writeFile(path.join(out, `${scenario}-failure.txt`), await page.locator('body').innerText().catch(() => 'Page unavailable'));
        item.fixtureFailures = await page.evaluate(() => window.__recordingE2E?.failures).catch(() => []);
        console.error(`FAIL ${scenario}: ${error.message}`);
        // Fresh contexts isolate failures: report all regressions, rather than
        // letting the same-ID Settings failure hide the different-ID/IPC bugs.
      } finally {
        await context.close();
      }
    }
    const failed = report.scenarios.filter((item) => item.status === 'failed');
    if (failed.length) throw new Error(`${failed.length}/${report.scenarios.length} scenarios failed: ${failed.map((item) => item.name).join(', ')}`);
    report.status = 'passed';
  } catch (error) {
    report.status = 'failed';
    report.error = String(error.stack || error);
    throw error;
  } finally {
    await browser?.close();
    await new Promise((resolve) => {
      if (child.exitCode !== null) return resolve();
      const timer = setTimeout(() => { child.kill(); }, 10000);
      child.once('exit', () => { clearTimeout(timer); resolve(); });
      if (child.connected) child.send('stop'); else child.kill();
    });
    report.serverStopped = child.exitCode !== null || child.signalCode !== null;
    await writeFile(path.join(out, 'report.json'), JSON.stringify(report, null, 2));
    await writeFile(path.join(out, 'vite.log'), serverLog);
    console.log(`Report: ${path.join(out, 'report.json')}`);
  }
}

if (args.includes('--server')) await serve();
else await run();
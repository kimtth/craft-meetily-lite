import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import ts from 'typescript';

// Run directly with node --test; no test framework or cloud SDK is required.
async function loadTypeScript(relativePath) {
  const source = await readFile(new URL(relativePath, import.meta.url), 'utf8');
  const { outputText } = ts.transpileModule(source, {
    compilerOptions: { target: ts.ScriptTarget.ES2020, module: ts.ModuleKind.ESNext },
  });
  return import(`data:text/javascript;base64,${Buffer.from(outputText).toString('base64')}`);
}

const { PcmTimeline } = await loadTypeScript('./pcmTimeline.ts');
const { normalizeLanguage, normalizeAzureLanguage, normalizeFoundryLanguage, AZURE_LANGUAGE_OPTIONS } =
  await loadTypeScript('./recordingOptions.ts');

test('Azure locale casing round-trips every offered language and preserves regional variants', () => {
  for (const { value } of AZURE_LANGUAGE_OPTIONS) {
    assert.equal(normalizeAzureLanguage(` ${value.toLowerCase().replace('-', '_')} `), value);
    assert.equal(normalizeAzureLanguage(value.split('-')[0]), value);
    assert.equal(normalizeFoundryLanguage(value), value.split('-')[0]);
  }
  assert.equal(normalizeAzureLanguage('EN_gb'), 'en-GB');
  assert.equal(normalizeLanguage('sr_latn_rs'), 'sr-Latn-RS');
  assert.equal(normalizeAzureLanguage('auto'), 'en-US');
  assert.equal(normalizeAzureLanguage(''), 'en-US');
  assert.equal(normalizeFoundryLanguage('AUTO'), 'auto');
  assert.equal(normalizeLanguage(null), 'en');
});

test('legacy PCM pushes retain zero-based SDK offsets', () => {
  const timeline = new PcmTimeline();
  assert.equal(timeline.append(32_000), 0);
  timeline.append(32_000);
  assert.equal(timeline.offsetSeconds(15_000_000), 1.5);
});

test('first PCM offset supplies absolute append and reconnect base', () => {
  const timeline = new PcmTimeline();
  timeline.append(32_000, 123.25);
  timeline.append(32_000, 124.25);
  assert.equal(timeline.offsetSeconds(0), 123.25);
  assert.equal(timeline.offsetSeconds(15_000_000), 124.75);
});

test('recognizer restart starts at its own first PCM, not zero or UI duration', () => {
  const beforeRestart = new PcmTimeline();
  beforeRestart.append(32_000, 10);
  const afterRestart = new PcmTimeline();
  // Audio captured during recognizer reconfiguration was not pushed to it.
  afterRestart.append(32_000, 14.5);
  assert.equal(beforeRestart.offsetSeconds(5_000_000), 10.5);
  assert.equal(afterRestart.offsetSeconds(5_000_000), 15);
});

test('interior gaps map by PCM anchors without changing earlier delayed results', () => {
  const timeline = new PcmTimeline();
  timeline.append(32_000, 20);
  timeline.append(32_000, 24);
  timeline.append(32_000, 30);
  assert.equal(timeline.offsetSeconds(5_000_000), 20.5);
  assert.equal(timeline.offsetSeconds(10_000_000), 24);
  assert.equal(timeline.offsetSeconds(15_000_000), 24.5);
  assert.equal(timeline.offsetSeconds(20_000_000), 30);
  assert.equal(timeline.offsetSeconds(25_000_000), 30.5);
});

test('duplicates and partial overlaps do not replay PCM or shift the clock backwards', () => {
  const timeline = new PcmTimeline();
  timeline.append(32_000, 10);
  assert.equal(timeline.append(32_000, 10), 16_000);
  assert.equal(timeline.append(32_000, 10.5), 8_000);
  assert.equal(timeline.offsetSeconds(12_500_000), 11.25);
  timeline.append(32_000);
  assert.equal(timeline.offsetSeconds(20_000_000), 12);
});

test('empty PCM cannot incorrectly establish a session base', () => {
  const timeline = new PcmTimeline();
  timeline.append(0, 1);
  timeline.append(2, 7);
  assert.equal(timeline.offsetSeconds(0), 7);
});

test('sample-count clock remains exact across many fractional-second callbacks', () => {
  const timeline = new PcmTimeline();
  const baseSamples = 320_007;
  const chunkSamples = 127;
  for (let index = 0; index < 10_000; index += 1) {
    assert.equal(timeline.append(chunkSamples * 2, (baseSamples + index * chunkSamples) / 16_000), 0);
  }
  const elapsedSamples = 1_269_873;
  assert.equal(timeline.offsetSeconds(elapsedSamples * 625), (baseSamples + elapsedSamples) / 16_000);
});

test('invalid offsets and incomplete PCM fail without corrupting prior anchors', () => {
  const timeline = new PcmTimeline();
  timeline.append(32_000, 5);
  for (const offset of [-1, NaN, Infinity, Number.MAX_VALUE]) {
    assert.throws(() => timeline.append(2, offset), /offset/i);
  }
  for (const bytes of [-1, 1, 1.5, NaN]) {
    assert.throws(() => timeline.append(bytes, 6), /PCM16/i);
  }
  assert.equal(timeline.offsetSeconds(5_000_000), 5.5);
});
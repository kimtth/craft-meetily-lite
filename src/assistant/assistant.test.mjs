import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import ts from 'typescript';

// Compile the production pure helpers, not a second implementation or an SDK.
const text = await readFile(new URL('./MeetingAssistant.tsx', import.meta.url), 'utf8');
const tree = ts.createSourceFile('MeetingAssistant.tsx', text, ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
const names = new Set(['captureAssistantContext', 'isAssistantContextAppend', 'assistantCoverageLabel', 'shouldSendAssistantKey', 'assistantModelLabel', 'timestamp']);
const source = tree.statements.filter(node => ts.isFunctionDeclaration(node) && names.has(node.name?.text))
  .map(node => node.getText(tree)).join('\n');
const { outputText } = ts.transpileModule(source, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext } });
const helpers = await import(`data:text/javascript;base64,${Buffer.from(outputText).toString('base64')}`);
const capture = helpers.captureAssistantContext;
const meeting = () => ({ id: 'a', title: 'Synthetic', language: 'ja-JP', updatedAt: 'before', durationSeconds: 20,
  hasAudio: false, transcript: [{ id: 's1', offsetSeconds: 5, text: 'Synthetic fact', speakerId: '0' }] });

test('model picker labels Auto honestly and preserves all runtime-returned model IDs', () => {
  assert.equal(helpers.assistantModelLabel({ id: 'auto', name: 'Auto' }), 'Auto — GitHub selects the model');
  assert.equal(helpers.assistantModelLabel({ id: 'synthetic-model', name: 'Synthetic' }), 'Synthetic (synthetic-model)');
  assert.match(text, /status\?\.models\.map\(candidate => <option key=\{candidate.id\} value=\{candidate.id\}>\{assistantModelLabel\(candidate\)\}/);
});

test('Copilot sharing is authorized only in the explicit submission handler, without a checkbox gate', () => {
  const asks = [];
  function visit(node) {
    if (ts.isCallExpression(node) && node.expression.getText(tree) === 'invoke'
      && node.arguments[0]?.getText(tree) === "'ask_copilot'") asks.push(node);
    ts.forEachChild(node, visit);
  }
  visit(tree);
  assert.equal(asks.length, 1);
  let owner = asks[0].parent;
  while (owner && !ts.isFunctionDeclaration(owner)) owner = owner.parent;
  assert.equal(owner?.name?.text, 'ask');
  const payload = asks[0].arguments[1];
  assert.ok(ts.isObjectLiteralExpression(payload));
  const authorization = payload.properties.find(node => node.name?.getText(tree) === 'consent');
  assert.equal(authorization?.initializer?.kind, ts.SyntaxKind.TrueKeyword);
  assert.doesNotMatch(text, /setConsent|ma-consent|give consent/);
  assert.match(text, /const configured = !!status\?\.authenticated && !!selectedModel;/);
});

test('confirmed speech appends and recording bookkeeping preserve pending response context', () => {
  const before = meeting();
  const after = structuredClone(before);
  after.updatedAt = 'after'; after.durationSeconds = 40; after.hasAudio = true; after.recordingPath = 'synthetic.wav';
  after.transcript.push({ id: 's2', offsetSeconds: 30, text: 'More confirmed speech' });
  assert.equal(helpers.isAssistantContextAppend(capture(before), capture(after)), true);
});

test('edits, deletes, speaker renames, duplicate IDs and another meeting are not append-only', () => {
  const before = meeting();
  for (const change of [m => { m.transcript[0].text = 'Edited'; }, m => { m.transcript = []; },
    m => { m.transcript[0].speakerName = 'New name'; }, m => { m.id = 'b'; },
    m => { m.transcript.push(structuredClone(m.transcript[0])); }]) {
    const after = structuredClone(before); change(after);
    assert.equal(helpers.isAssistantContextAppend(capture(before), capture(after)), false);
  }
});

test('coverage describes included segment start, never current recording duration', () => {
  assert.equal(helpers.assistantCoverageLabel({ contextSegmentCount: 3, contextThroughSeconds: 65.8 }), 'Transcript through 01:05 · 3 segments');
  assert.equal(helpers.assistantCoverageLabel({}), 'Transcript coverage unavailable');
  assert.equal(helpers.assistantCoverageLabel({ contextSegmentCount: 1, contextThroughSeconds: -1 }), 'Transcript coverage unavailable');
});

test('Enter sends but Shift+Enter and Japanese/Korean IME composition do not', () => {
  const key = { key: 'Enter', shiftKey: false, isComposing: false, keyCode: 13 };
  assert.equal(helpers.shouldSendAssistantKey(key), true);
  for (const patch of [{ shiftKey: true }, { isComposing: true }, { keyCode: 229 }, { key: 'a' }]) {
    assert.equal(helpers.shouldSendAssistantKey({ ...key, ...patch }), false);
  }
});
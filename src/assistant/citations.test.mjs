import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import ts from 'typescript';
import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';

// Execute the production parser AND React renderers offline, not a copy.
const source = await readFile(new URL('./Citations.tsx', import.meta.url), 'utf8');
const { outputText } = ts.transpileModule(source, { compilerOptions: {
  target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext, jsx: ts.JsxEmit.ReactJSX,
} });
const compiled = outputText.replaceAll('"react/jsx-runtime"', JSON.stringify(import.meta.resolve('react/jsx-runtime')))
  .replaceAll("'react'", JSON.stringify(import.meta.resolve('react')));
const { citationParts, sortedCitationIds, CitationText, CitationSources, CitationReference } =
  await import(`data:text/javascript;base64,${Buffer.from(compiled).toString('base64')}`);
const sources = new Map([
  ['e10', { id: 'e10', offsetSeconds: 70.25, text: 'Later synthetic statement' }],
  ['e1', { id: 'e1', offsetSeconds: 5.5, text: '早い発言 <img src=x onerror=alert(1)>' }],
  ['source-3', { id: 'source-3', offsetSeconds: 3661, text: 'Last synthetic statement' }],
]);
const defaults = { text: 'First [[cite:v1:1,2]] next [[cite:v1:2]]', sourceIds: ['e10', 'e1'], sources, disabled: false, onSeek() {} };
const render = (Component, props = {}) => renderToStaticMarkup(createElement(Component, { ...defaults, ...props }));

test('new versioned markers resolve only saved field-local positions, including e1/e10 ID collisions', () => {
  assert.deepEqual(citationParts(defaults.text, defaults.sourceIds), [
    { kind: 'text', text: 'First ' }, { kind: 'sources', ids: ['e10', 'e1'] },
    { kind: 'text', text: ' next ' }, { kind: 'sources', ids: ['e1'] },
  ]);
  const html = render(CitationText);
  assert.ok(html.indexOf('00:05') < html.indexOf('01:10'), 'each claim reference group sorts by timestamp');
  assert.equal((html.match(/<button/g) ?? []).length, 3);
  assert.doesNotMatch(html, /cite:v1/);
});

test('legacy Japanese/fullwidth alias groups stay unresolved even when original IDs are e1/e10', () => {
  for (const text of ['決定［e1、e10］', '決定【e1，e99】', '決定[e1, e10]', '決定［ｅ１、ｅ１０］']) {
    const props = { ...defaults, text };
    const before = JSON.stringify(props);
    assert.deepEqual(citationParts(text, defaults.sourceIds), [{ kind: 'text', text: '決定' }, { kind: 'unresolved' }]);
    const html = render(CitationText, props);
    assert.match(html, /Reference unavailable/);
    assert.doesNotMatch(html, /<button/);
    assert.equal(JSON.stringify(props), before, 'display never edits saved data');
    assert.match(render(CitationSources, props), /Claim-level reference mapping unavailable/);
  }
});

test('unknown, partial, excessive, malformed and future markers fail closed', () => {
  for (const text of ['[[cite:v1:1,3]]', '[[cite:v1:0]]', '[[cite:v1:01]]', '[[cite:v1:1,e1]]',
    '[[cite:v1:9007199254740992]]', '[[cite:v2:1]]', '[[cite:v1:1,]]', `[[cite:v1:${Array(101).fill(1).join(',')}]]`]) {
    assert.deepEqual(citationParts(text, defaults.sourceIds), [{ kind: 'unresolved' }], text);
    assert.doesNotMatch(render(CitationText, { text }), /<button/);
  }
  assert.deepEqual(citationParts('[[cite:v1:1]]', []), [{ kind: 'unresolved' }]);
});

test('no markers and bare eN remain prose; structured sources do not invent claim mapping', () => {
  const text = 'Bare e1 and e10 [ordinary prose] [June10] [release1] [資料e1] 😀';
  assert.deepEqual(citationParts(text, defaults.sourceIds), [{ kind: 'text', text }]);
  assert.equal(render(CitationText, { text }), text);
  const html = render(CitationSources, { text });
  assert.match(html, /<details class="ma-source-list"><summary>Sources \(2\)<\/summary>/);
  assert.doesNotMatch(html, /<details[^>]*open/);
  assert.match(html, /Claim-level reference mapping unavailable/);
  assert.equal(render(CitationSources, { text, sourceIds: [] }), '');
});

test('reference lists deduplicate and sort timestamps without mutating stored sourceIds', () => {
  const ids = ['source-3', 'missing', 'e10', 'e1', 'e10'];
  const before = [...ids];
  assert.deepEqual(sortedCitationIds(ids, sources), ['e1', 'e10', 'source-3', 'missing']);
  assert.deepEqual(ids, before);
  assert.match(render(CitationSources, { sourceIds: ids }), /Sources \(4\)/);
  assert.match(render(CitationSources, { sourceIds: ids }), /1:01:01/);
});

test('click uses exact source offset and disabled/stale references keep preview but never seek', () => {
  const seeks = [];
  const props = { id: 'e10', sources, onSeek: value => seeks.push(value) };
  for (const disabled of [false, true]) {
    const node = CitationReference({ ...props, disabled });
    const button = node.props.children[0];
    assert.equal(button.props.disabled, disabled);
    button.props.onClick();
    assert.equal(node.props.title, 'Later synthetic statement');
    assert.equal(node.props.tabIndex, disabled ? 0 : undefined);
  }
  assert.deepEqual(seeks, [70.25]);
  const html = render(CitationText, { disabled: true });
  assert.equal((html.match(/disabled=""/g) ?? []).length, 3);
  assert.match(html, /Playback unavailable/);
});

test('missing, ambiguous and invalid transcript sources never become clickable', () => {
  const invalid = new Map([['e10', null], ['e1', { id: 'e1', offsetSeconds: NaN, text: 'Bad' }]]);
  for (const offsetSeconds of [NaN, Infinity, -1]) {
    invalid.set('e1', { id: 'e1', offsetSeconds, text: 'Bad' });
    const html = render(CitationText, { sources: invalid });
    assert.doesNotMatch(html, /<button/);
    assert.match(html, /Reference unavailable/);
  }
  assert.doesNotMatch(render(CitationText, { sources: new Map() }), /<button/);
});

test('rendering escapes claim text and previews, with no raw HTML or inferred alias lookup', async () => {
  const html = render(CitationText, { text: '<script>alert(1)</script> [[cite:v1:2]]' });
  assert.match(html, /&lt;script&gt;/);
  assert.match(html, /&lt;img/);
  assert.doesNotMatch(html, /<script|<img/);
  assert.doesNotMatch(source, /dangerouslySetInnerHTML|innerHTML/);
  const panel = await readFile(new URL('./MeetingAssistant.tsx', import.meta.url), 'utf8');
  assert.match(panel, /disabled: disabled \|\| playbackDisabled \|\| !!stale/);
  assert.match(panel, /message.role === 'assistant'\s*\? <CitationText/);
});
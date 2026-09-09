import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { createRequire } from 'node:module';
import test from 'node:test';
import vm from 'node:vm';
import React from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import ts from 'typescript';

const source = await readFile(new URL('./MeetingAssistant.tsx', import.meta.url), 'utf8');
const css = await readFile(new URL('./assistant.css', import.meta.url), 'utf8');
const require = createRequire(import.meta.url);
const { outputText } = ts.transpileModule(source, { compilerOptions: {
  target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX,
} });
const module = { exports: {} };
vm.runInNewContext(outputText, {
  module, exports: module.exports, URLSearchParams, TextEncoder,
  window: { location: { search: '' }, matchMedia: () => ({ matches: false }) },
  document: { documentElement: { getAttribute: () => 'light' } },
  require: name => {
    if (name === './assistant.css') return {};
    if (name === './Citations') return { CitationText: () => null, CitationSources: () => null };
    if (name === '@tauri-apps/api/core') return {
      isTauri: () => false, invoke: () => { throw new Error('Native/network calls forbidden in layout tests'); },
    };
    return require(name);
  },
});
const markup = renderToStaticMarkup(React.createElement(module.exports.MeetingAssistant, {
  meeting: { id: 'synthetic-layout', title: 'Synthetic only', language: 'en-US',
    updatedAt: 'synthetic', durationSeconds: 0, hasAudio: false, transcript: [] },
  onSeek: () => {},
}));

test('initial layout keeps model and Check status visible outside collapsed details', () => {
  const controls = markup.indexOf('class="ma-model-controls"');
  const connection = markup.indexOf('class="ma-connection"');
  assert.ok(controls >= 0 && controls < connection);
  assert.match(markup.slice(controls, connection), /Model for next request/);
  assert.match(markup.slice(controls, connection), />Check status<\/button>/);
  assert.match(markup, /class="ma-setup-toggle" aria-expanded="false"/);
  assert.match(markup, /class="ma-connection" hidden="" role="region" aria-label="Connection and sharing details" tabindex="0"/);
  assert.equal((markup.match(/<select /g) ?? []).length, 2);
  assert.match(markup.slice(connection), /GitHub account for Meetly/);
});

test('repeated sharing sentence is removed and clear requires confirmation', () => {
  const conversation = markup.indexOf('class="ma-chat-scroll"');
  const suggestions = markup.indexOf('class="ma-suggestions"');
  const composer = markup.indexOf('class="ma-composer"');
  assert.ok(conversation >= 0 && suggestions > conversation && composer > suggestions);
  const form = markup.slice(composer, markup.indexOf('</form>', composer));
  assert.match(form, /<button\b(?=[^>]*aria-label="Send to meeting assistant")(?=[^>]*disabled="")[^>]*>/);
  assert.doesNotMatch(markup, /Send \/ Generate shares transcript|ma-composer-disclosure/);
  assert.match(markup, />Clear session<\/button>/);
  assert.doesNotMatch(markup, /role="alertdialog"/);
  assert.match(source, /if \(!confirmClear \|\| !begin\('clear'\)\) return/);
  assert.match(source, /invoke<AssistantState>\('clear_meeting_assistant', \{ meetingId: meeting.id \}\)/);
});

test('CSS contract: details are bounded in flow; conversation scrolls independently', () => {
  const connection = css.match(/\.meeting-assistant \.ma-connection \{([^}]+)\}/)?.[1];
  assert.ok(connection);
  assert.doesNotMatch(connection, /position:\s*(absolute|fixed)|z-index|box-shadow/);
  assert.match(connection, /max-height:\s*min\(16cqh, 104px\)/);
  assert.match(connection, /overflow-y:\s*auto/);
  assert.match(css, /\.ma-chat-scroll \{[^}]*min-height:\s*0;[^}]*overflow-y:\s*auto/);
  assert.match(css, /\.ma-composer \{\s*flex:\s*none/);
});

test('CSS contract: small dock sizing and wrapped suggestions avoid horizontal scrolling', () => {
  assert.match(css, /container-type:\s*size/);
  assert.match(css, /@container \(max-width: 360px\)/);
  assert.match(css, /@container \(max-height: 650px\)/);
  assert.match(css, /\.ma-model-controls \{[^}]*grid-template-columns:\s*minmax\(0, 1fr\) auto/);
  assert.match(css, /\.ma-suggestions \{[^}]*flex-wrap:\s*wrap/);
  assert.doesNotMatch(css, /overflow-x:\s*(auto|scroll)/);
});
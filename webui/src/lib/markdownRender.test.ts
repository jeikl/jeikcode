import assert from 'node:assert/strict';
import { test } from 'node:test';
import { markdownToHtml } from './markdownRender.ts';

test('GitHub alerts become classed blockquotes', () => {
  const note = markdownToHtml('> [!NOTE]\n> hello');
  assert.match(note, /md-alert md-alert-note/);
  assert.match(note, /md-alert-title/);
  assert.match(note, /hello/);

  const warn = markdownToHtml('> [!WARNING] be careful');
  assert.match(warn, /md-alert-warning/);
  assert.match(warn, /be careful/);
});

test('code fence language sentinel is stripped and generic lang is hidden', () => {
  const out = markdownToHtml('```text\ntext\nBase URL: http://127.0.0.1:8045\n```');
  assert.match(out, /Base URL: http:\/\/127\.0\.0\.1:8045/);
  assert.doesNotMatch(out, /<code[^>]*>text\nBase URL/);
  assert.doesNotMatch(out, /<span class="code-block-lang">text<\/span>/);
});

test('named language gets a toolbar label', () => {
  const out = markdownToHtml('```ts\nconst n = 1;\n```');
  assert.match(out, /<span class="code-block-lang">ts<\/span>/);
  assert.match(out, /const n = 1;/);
});

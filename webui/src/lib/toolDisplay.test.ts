import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  jsonArgString,
  looksLikeUnifiedDiff,
  parseDiffPreview,
  formatToolPayload,
  prettyToolText,
  structuredToolFields,
  toolCategory,
  toolGlyph,
  toolRendersAsDiff,
} from './toolDisplay.ts';

test('tool glyphs match OpenCode classes', () => {
  assert.equal(toolGlyph('bash'), '$');
  assert.equal(toolGlyph('edit_file'), '←');
  assert.equal(toolGlyph('write_file'), '←');
  assert.equal(toolGlyph('read_file'), '→');
  assert.equal(toolGlyph('grep'), '✱');
  assert.equal(toolGlyph('code_explore'), '✱');
  assert.equal(toolGlyph('web_search'), '◈');
  assert.equal(toolCategory('bash'), 'terminal');
  assert.equal(toolCategory('mcp__srv__tool'), 'mcp');
});

test('jsonArgString reads a string field', () => {
  assert.equal(jsonArgString('{"command":"ls -la"}', 'command'), 'ls -la');
  assert.equal(jsonArgString('not-json', 'command'), '');
});

test('parseDiffPreview colors add/del and ignores non-diff output', () => {
  const lines = parseDiffPreview(
    [
      'diff --git a/foo.rs b/foo.rs',
      '--- a/foo.rs',
      '+++ b/foo.rs',
      '@@ -1,2 +1,2 @@',
      ' fn main() {',
      '-    let x = 1;',
      '+    let x = 2;',
      ' }',
    ].join('\n'),
  );
  assert.equal(lines[0].kind, 'meta');
  const del = lines.find((l) => l.kind === 'del');
  const add = lines.find((l) => l.kind === 'add');
  assert.ok(del && del.text.includes('let x = 1') && del.oldLine === 2);
  assert.ok(add && add.text.includes('let x = 2') && add.newLine === 2);
  assert.deepEqual(parseDiffPreview('Created new file foo.rs (12 bytes)'), []);
  assert.ok(
    looksLikeUnifiedDiff(
      [
        'diff --git a/foo.rs b/foo.rs',
        '--- a/foo.rs',
        '+++ b/foo.rs',
        '@@ -1,2 +1,2 @@',
        ' fn main() {',
        '-    let x = 1;',
        '+    let x = 2;',
        ' }',
      ].join('\n'),
    ),
  );
  assert.equal(looksLikeUnifiedDiff('Created new file foo.rs (12 bytes)'), false);
});

test('bullet lists and code_explore output are not treated as diffs', () => {
  const explore = [
    "- F3 'memory/黄金sql案例/foo.sql'",
    "- F16 'memory/bar.md'",
    '> 📂 **Directory Panorama**',
    '- more/files',
  ].join('\n');
  assert.equal(looksLikeUnifiedDiff(explore), false);
  assert.deepEqual(parseDiffPreview(explore), []);
  assert.equal(toolRendersAsDiff('code_explore'), false);
  assert.equal(toolRendersAsDiff('edit_file'), true);
  assert.equal(toolRendersAsDiff('bash'), true);
});

test('formatToolPayload pretty-prints JSON for copyable code blocks', () => {
  const raw = JSON.stringify({
    file_path: 'test_demo_3.json',
    content: '{\n  "name": "tui-test"\n}\n',
  });
  const formatted = formatToolPayload(raw);
  assert.ok(formatted.includes('"file_path": "test_demo_3.json"'));
  assert.ok(formatted.includes('tui-test'));
  const pretty = prettyToolText(raw);
  assert.equal(pretty.lang, 'json');
  const fields = structuredToolFields(raw);
  assert.equal(fields?.[0]?.key, 'file_path');
  assert.ok(fields?.some((f) => f.key === 'content' && f.multiline));
  assert.equal(prettyToolText('- F3 memory/foo').lang, 'text');
});

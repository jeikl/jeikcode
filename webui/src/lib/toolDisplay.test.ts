import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  jsonArgString,
  looksLikeUnifiedDiff,
  parseDiffPreview,
  buildEditArgsDiff,
  resolveToolDiffPreview,
  formatToolPayload,
  prettyToolText,
  structuredToolFields,
  toolCategory,
  toolGlyph,
  toolRendersAsDiff,
  computeToolDiffStats,
  collectTurnDiffSummary,
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

test('buildEditArgsDiff colors old/new args when output has no unified diff', () => {
  const oldStr = '| **常规查询** | `sql-server-company` |\n| **通话相关** | `sql-server-company` |';
  const newStr = '| **常规查询** | `sql-server-company` |\n| **微信相关** | `pgsql-company` |';
  const lines = buildEditArgsDiff(oldStr, newStr);
  assert.ok(lines.some((l) => l.kind === 'del'));
  assert.ok(lines.some((l) => l.kind === 'add'));
  const resolved = resolveToolDiffPreview(
    'edit_file',
    'edit_file: hunk 1/1 failed. old_string not found.',
    JSON.stringify({ old_string: oldStr, new_string: newStr }),
  );
  assert.ok(resolved);
  assert.ok(resolved!.lines.some((l) => l.kind === 'del'));
  assert.ok(resolved!.lines.some((l) => l.kind === 'add'));
  assert.equal(resolved!.source, 'args');
});

test('resolveToolDiffPreview prefers output unified diff over args', () => {
  const output = [
    'Edited foo.md (1 replacement)',
    '@@ -1,2 +1,2 @@',
    ' ctx',
    '- old',
    '+ new',
  ].join('\n');
  const args = JSON.stringify({ old_string: 'other', new_string: 'ignored' });
  const resolved = resolveToolDiffPreview('edit_file', output, args);
  assert.ok(resolved);
  assert.equal(resolved!.source, 'output');
  assert.ok(resolved!.raw.includes('@@ -1,2'));
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

test('computeToolDiffStats calculates additions and deletions from diff and write_file', () => {
  const diffOutput = [
    'diff --git a/foo.rs b/foo.rs',
    '--- a/foo.rs',
    '+++ b/foo.rs',
    '@@ -1,3 +1,4 @@',
    ' unchanged',
    '-deleted line 1',
    '-deleted line 2',
    '+added line 1',
    '+added line 2',
    '+added line 3',
  ].join('\n');
  const stats = computeToolDiffStats('edit_file', diffOutput, undefined);
  assert.deepEqual(stats, { additions: 3, deletions: 2 });

  const writeStats = computeToolDiffStats(
    'write_file',
    'Wrote 2 lines',
    JSON.stringify({ file_path: 'foo.txt', content: 'line 1\nline 2\nline 3' }),
  );
  assert.deepEqual(writeStats, { additions: 3, deletions: 0 });

  const noDiff = computeToolDiffStats('read_file', 'some text', JSON.stringify({ file_path: 'foo.txt' }));
  assert.equal(noDiff, null);
});

test('collectTurnDiffSummary aggregates files and lines across turn parts', () => {
  const parts = [
    { kind: 'text', text: 'starting edits' },
    {
      kind: 'tool',
      tool: {
        id: 'c1',
        name: 'edit_file',
        args: JSON.stringify({ file_path: 'a.rs' }),
        output: '@@ -1,2 +1,3 @@\n ctx\n-del\n+add1\n+add2',
      },
    },
    {
      kind: 'tool',
      tool: {
        id: 'c2',
        name: 'write_file',
        args: JSON.stringify({ file_path: 'b.rs', content: 'x\ny\nz' }),
        output: 'created b.rs',
      },
    },
    { kind: 'text', text: 'finished' },
  ];

  const summary = collectTurnDiffSummary(parts as any);
  assert.deepEqual(summary, {
    fileCount: 2,
    additions: 5, // 2 from a.rs + 3 from b.rs
    deletions: 1, // 1 from a.rs
    toolCount: 2,
  });
});

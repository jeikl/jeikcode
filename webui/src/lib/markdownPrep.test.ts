import assert from 'node:assert/strict';
import { test } from 'node:test';
import { marked } from 'marked';
import {
  buildTableDelimiter,
  isTableDelimiterLine,
  preprocessMarkdown,
  splitTableCells,
  stripLanguageSentinel,
} from './markdownPrep.ts';

marked.setOptions({ gfm: true, breaks: false });
marked.use({ tokenizer: { del: () => undefined } });

function html(markdown: string): string {
  return marked.parse(preprocessMarkdown(markdown)) as string;
}

test('splitTableCells drops wrapping pipes and keeps escaped pipes', () => {
  assert.deepEqual(splitTableCells('| 目标 | 协议 |'), ['目标', '协议']);
  assert.deepEqual(splitTableCells('|---|'), ['---']);
  assert.deepEqual(splitTableCells('| a \\| b | c |'), ['a | b', 'c']);
});

test('isTableDelimiterLine accepts one-column |---|', () => {
  assert.equal(isTableDelimiterLine('|---|'), true);
  assert.equal(isTableDelimiterLine('|---|---|'), true);
  assert.equal(isTableDelimiterLine('| 目标 | 协议 |'), false);
  assert.equal(isTableDelimiterLine('---'), true);
});

test('buildTableDelimiter pads to header width and keeps alignment', () => {
  assert.equal(buildTableDelimiter(2, ['---']), '| --- | --- |');
  assert.equal(buildTableDelimiter(2, [':---', '---:']), '| :--- | ---: |');
});

test('screenshot-like 2-col header with 1-col delimiter renders as a table', () => {
  const markdown = [
    '## 怎么选',
    '| 目标 | 协议 |',
    '|---|',
    '| 思考完整回传 + 多轮签名不断 | Gemini |',
    '| Claude Code /ic SDK |ic  /v1/  |',
    '| Cherry Studio / 通用 OpenAI 客户端，不在乎思考链 | OpenAI |',
    '客户端配置示例:',
  ].join('\n');
  const out = html(markdown);
  assert.match(out, /<table>/);
  assert.match(out, /<th>目标<\/th>/);
  assert.match(out, /<th>协议<\/th>/);
  assert.match(out, /<td>Gemini<\/td>/);
  assert.match(out, /<p>客户端配置示例:/);
  assert.doesNotMatch(out, /<td>客户端配置示例:/);
});

test('well-formed GFM table still renders and does not swallow following prose', () => {
  const out = html('| 目标 | 协议 |\n|---|---|\n| a | b |\n客户端配置示例:');
  assert.match(out, /<table>/);
  assert.match(out, /<td>a<\/td>/);
  assert.match(out, /<p>客户端配置示例:/);
});

test('missing delimiter between two pipe rows is inserted', () => {
  const out = html('| 目标 | 协议 |\n| a | Gemini |\n后面的话');
  assert.match(out, /<table>/);
  assert.match(out, /<td>Gemini<\/td>/);
  assert.match(out, /<p>后面的话<\/p>/);
});

test('bare --- under a pipe header becomes a table delimiter instead of setext', () => {
  const out = html('| A | B |\n---\n| x | y |');
  assert.match(out, /<table>/);
  assert.match(out, /<td>x<\/td>/);
  assert.doesNotMatch(out, /<h2>/);
});

test('ordinary setext heading is preserved', () => {
  const out = html('Heading\n---\nbody');
  assert.match(out, /<h2.*>Heading<\/h2>/);
});

test('prose containing a pipe plus --- stays a setext heading', () => {
  const out = html('run foo | grep bar\n---\nbody');
  assert.match(out, /<h2.*>run foo \| grep bar<\/h2>/);
  assert.doesNotMatch(out, /<table>/);
});

test('fullwidth pipes are normalized into a table', () => {
  const out = html('| 左 ｜ 右 |\n| --- ｜ --- |\n| a ｜ b |');
  assert.match(out, /<table>/);
  assert.match(out, /<th>左<\/th>/);
  assert.match(out, /<td>a<\/td>/);
});

test('tables inside fenced code are not rewritten', () => {
  const markdown = ['```markdown', '| A | B |', '|---|', '| x | y |', '```'].join('\n');
  const prepared = preprocessMarkdown(markdown);
  assert.match(prepared, /\|---\|/);
  const out = html(markdown);
  assert.doesNotMatch(out, /<table>/);
  assert.match(out, /<pre><code/);
});

test('heading without space after hashes is repaired', () => {
  const out = html('###怎么选\nhello');
  assert.match(out, /<h3.*>怎么选<\/h3>/);
});

test('unclosed fence is closed so trailing prose stays in the code block', () => {
  const out = html('intro\n```ts\nconst x = 1;');
  assert.match(out, /<pre><code class="language-ts">[\s\S]*const x = 1;/);
  assert.doesNotMatch(out, /<p>const x = 1;/);
});

test('==highlight== and footnotes render outside code', () => {
  const out = html('see ==hot== and a note[^1]\n\n[^1]: extra detail');
  assert.match(out, /<mark class="md-highlight">hot<\/mark>/);
  assert.match(out, /<sup class="md-footnote-ref">/);
  assert.match(out, /id="md-fn-/);
  assert.match(out, /extra detail/);
});

test('highlight and footnote syntax inside inline code stay literal', () => {
  const out = html('use `==hot==` and `[^1]`');
  assert.match(out, /<code>==hot==<\/code>/);
  assert.match(out, /<code>\[\^1\]<\/code>/);
});

test('tilde ranges are not turned into strikethrough', () => {
  const out = html('步骤 1~3 然后 4~7');
  assert.match(out, /步骤 1~3 然后 4~7/);
  assert.doesNotMatch(out, /<del>/);
});

test('task list checkboxes survive', () => {
  const out = html('- [ ] todo\n- [x] done');
  assert.match(out, /<input disabled="" type="checkbox">/);
  assert.match(out, /checked=""/);
});

test('stripLanguageSentinel drops a duplicated text first line', () => {
  assert.equal(stripLanguageSentinel('text\nBase URL: 1\n', 'text'), 'Base URL: 1\n');
  assert.equal(stripLanguageSentinel('text\nBase URL: 1', ''), 'Base URL: 1');
  assert.equal(stripLanguageSentinel('text', ''), 'text');
  assert.equal(stripLanguageSentinel('hello\nworld', 'text'), 'hello\nworld');
});


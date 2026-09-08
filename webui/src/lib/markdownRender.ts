import { marked } from 'marked';
import {
  preprocessMarkdown,
  shouldShowCodeLanguage,
  stripLanguageSentinel,
} from './markdownPrep.ts';

export { preprocessMarkdown } from './markdownPrep.ts';

marked.setOptions({ gfm: true, breaks: false });

// 关闭 GFM 单/双波浪号删除线：模型输出里 `~` 多是字面量（步骤区间 `1~3`、路径
// `~/projects`），而非删除线。开着的话 `步骤 1~3 …继续执行 4~7` 会被当成
// `1<del>3 …4</del>7`，吃掉波浪号且整段加删除线，与 TUI 显示不一致（issue #825）。
marked.use({ tokenizer: { del: () => undefined } });

const renderer = new marked.Renderer();

const ALERT_TITLES: Record<string, string> = {
  note: 'Note',
  tip: 'Tip',
  important: 'Important',
  warning: 'Warning',
  caution: 'Caution',
};

renderer.blockquote = function (quote: string) {
  const kindMatch = quote.match(
    /^\s*<p>\s*\[!(NOTE|TIP|IMPORTANT|WARNING|CAUTION)\][ \t]*/i,
  );
  if (!kindMatch) return `<blockquote>${quote}</blockquote>\n`;
  const kind = kindMatch[1].toLowerCase();
  let body = quote.replace(
    /^\s*<p>\s*\[!(NOTE|TIP|IMPORTANT|WARNING|CAUTION)\][ \t]*/i,
    '<p>',
  );
  body = body.replace(/^\s*<p>\s*<\/p>\s*/, '');
  body = body.replace(/^\s*<p>\s+/, '<p>');
  const title = ALERT_TITLES[kind] ?? kindMatch[1];
  return (
    `<blockquote class="md-alert md-alert-${kind}">` +
    `<p class="md-alert-title">${title}</p>${body}</blockquote>\n`
  );
};

renderer.code = function (code: string, infostring?: string) {
  let text = code ?? '';
  if (!text.trim()) return '';
  const lang = (infostring ?? '').split(/\s+/)[0] ?? '';
  text = stripLanguageSentinel(text, lang);
  if (!text.trim()) return '';
  const esc = text.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
  const showLang = shouldShowCodeLanguage(lang);
  const langLabel = showLang
    ? `<span class="code-block-lang">${lang.replace(/[<>&"]/g, '')}</span>`
    : '<span class="code-block-lang"></span>';
  return (
    `<div class="code-block-wrapper${showLang ? ' has-language' : ''}">` +
    `<div class="code-block-toolbar">${langLabel}` +
    `<button class="copy-button" type="button" data-copy="${encodeURIComponent(text)}">Copy</button>` +
    `</div>` +
    `<pre><code class="${lang ? `language-${lang}` : ''}">${esc}</code></pre>` +
    `</div>`
  );
};

export function markdownToHtml(content: string): string {
  const preprocessed = preprocessMarkdown(content ?? '');
  return marked.parse(preprocessed, { renderer }) as string;
}

import { marked } from 'marked';
import DOMPurify from 'dompurify';
import { useMemo } from 'preact/hooks';

marked.setOptions({ gfm: true, breaks: false });

// 关闭 GFM 单/双波浪号删除线：模型输出里 `~` 多是字面量（步骤区间 `1~3`、路径
// `~/projects`），而非删除线。开着的话 `步骤 1~3 …继续执行 4~7` 会被当成
// `1<del>3 …4</del>7`，吃掉波浪号且整段加删除线，与 TUI 显示不一致（issue #825）。
// 覆盖 del tokenizer 返回 undefined → 波浪号回退为普通文本。
marked.use({ tokenizer: { del: () => undefined } });

// Custom code-block renderer → matches the ported .code-block-wrapper CSS.
const renderer = new marked.Renderer();

/**
 * 鲁棒性 Markdown 预处理器：
 * 解决大模型生成 Markdown 时常见的格式瑕疵，避免 marked 解析退化为纯文本：
 * 1. 保护代码块：被 ``` 或 ~~~ 包裹的行不作篡改；
 * 2. 标题规范化：
 *    - 补全 `#` 后的空格：如 `###三、修复方案` -> `### 三、修复方案`（CommonMark 严格要求 # 后跟空白符）；
 *    - 保证标题前有空行，防止被上一段落文本吞并；
 * 3. GFM 表格规范化：
 *    - 保证表格前有空行：GFM 规定表格不能中断普通文本段落或引用块（缺少空行会导致整个表格渲染失败）；
 *    - 保证表格后有空行：避免紧跟表格的文本破坏表格结构；
 *    - 支持宽松表格分隔行（如 `|---|---|`、`---|---|` 等）；
 */
export function preprocessMarkdown(raw: string): string {
  if (!raw) return '';
  const lines = raw.replace(/\r\n/g, '\n').split('\n');
  const result: string[] = [];
  let inCodeBlock = false;
  let codeFence = '';

  // 辅助检测 GFM 表格分隔行，例如 `| :--- | :--- |` 或 `|---|---|` 或 `---|---|`
  const isTableDelimiter = (line: string): boolean => {
    const trimmed = line.trim();
    if (!trimmed.includes('-')) return false;
    return /^\|?\s*:?-+:?\s*(\|?\s*:?-+:?\s*)+\|?$/.test(trimmed);
  };

  // 辅助检测可能为表格数据行
  const isTableRow = (line: string): boolean => {
    const trimmed = line.trim();
    return trimmed.startsWith('|') || (trimmed.endsWith('|') && trimmed.includes('|'));
  };

  for (let i = 0; i < lines.length; i++) {
    let line = lines[i];
    const trimmed = line.trim();

    // 1. 处理代码块围栏
    const fenceMatch = trimmed.match(/^(`{3,}|~{3,})/);
    if (fenceMatch) {
      if (!inCodeBlock) {
        inCodeBlock = true;
        codeFence = fenceMatch[1][0]; // '`' or '~'
      } else if (trimmed.startsWith(codeFence.repeat(3))) {
        inCodeBlock = false;
        codeFence = '';
      }
      result.push(line);
      continue;
    }

    if (inCodeBlock) {
      result.push(line);
      continue;
    }

    // 2. 修复无空格 ATX 标题，例如 `###三、修复方案` -> `### 三、修复方案`
    const headingMatch = line.match(/^(\s*)(#{1,6})([^\s#].*)$/);
    if (headingMatch) {
      line = `${headingMatch[1]}${headingMatch[2]} ${headingMatch[3]}`;
    }

    // 保证独立标题行前有空行
    const isHeading = /^\s*#{1,6}\s+/.test(line);
    if (isHeading && result.length > 0) {
      const prevLine = result[result.length - 1].trim();
      if (prevLine !== '' && !prevLine.startsWith('#')) {
        result.push('');
      }
    }

    // 3. 修复 GFM 表格前缺少空行的问题
    // 如果下一行是表格分隔行，说明当前行是表头
    const nextLine = i + 1 < lines.length ? lines[i + 1] : null;
    if (nextLine && isTableDelimiter(nextLine) && (line.includes('|') || isTableRow(line))) {
      // 当前行是表头！检查上一行是否为空行
      if (result.length > 0) {
        const prev = result[result.length - 1].trim();
        if (prev !== '' && !isTableRow(prev)) {
          // 在表头前强行插入一个空行，激活 marked 的 GFM 表格解析
          result.push('');
        }
      }
    }

    result.push(line);

    // 4. 检查表格结束后是否紧贴非表格文本
    if (isTableRow(line) || isTableDelimiter(line)) {
      if (nextLine !== null) {
        const nextTrimmed = nextLine.trim();
        if (nextTrimmed !== '' && !isTableRow(nextTrimmed) && !isTableDelimiter(nextTrimmed)) {
          // 表格紧跟文本，插入空行
          result.push('');
        }
      }
    }
  }

  return result.join('\n');
}
renderer.code = function (code: string, infostring?: string) {
  const text = code ?? '';
  if (!text.trim()) return '';
  const lang = (infostring ?? '').split(/\s+/)[0] ?? '';
  // Escape HTML in code; syntax highlighting is intentionally omitted here.
  const esc = text.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
  return (
    `<div class="code-block-wrapper">` +
    `<pre><code class="${lang ? `language-${lang}` : ''}">${esc}</code></pre>` +
    `<button class="copy-button" type="button" data-copy="${encodeURIComponent(text)}">Copy</button>` +
    `</div>`
  );
};

function highlightHtml(html: string, search: string): string {
  if (!search.trim()) return html;
  const parser = new DOMParser();
  const doc = parser.parseFromString(html, 'text/html');
  const walk = doc.createTreeWalker(doc.body, NodeFilter.SHOW_TEXT, null);
  const nodes: Text[] = [];
  let node: Node | null;
  while ((node = walk.nextNode())) {
    nodes.push(node as Text);
  }
  const searchLower = search.toLowerCase();
  for (const textNode of nodes) {
    const text = textNode.nodeValue ?? '';
    if (!text.toLowerCase().includes(searchLower)) continue;
    
    const parent = textNode.parentNode;
    if (!parent) continue;
    
    const parentTag = (parent as HTMLElement).tagName?.toLowerCase();
    if (parentTag === 'script' || parentTag === 'style') continue;

    const newFragment = doc.createDocumentFragment();
    let lastIndex = 0;
    let index = text.toLowerCase().indexOf(searchLower);
    while (index !== -1) {
      if (index > lastIndex) {
        newFragment.appendChild(doc.createTextNode(text.substring(lastIndex, index)));
      }
      const mark = doc.createElement('mark');
      mark.className = 'msg-search-highlight';
      mark.textContent = text.substring(index, index + search.length);
      newFragment.appendChild(mark);
      lastIndex = index + search.length;
      index = text.toLowerCase().indexOf(searchLower, lastIndex);
    }
    if (lastIndex < text.length) {
      newFragment.appendChild(doc.createTextNode(text.substring(lastIndex)));
    }
    parent.replaceChild(newFragment, textNode);
  }
  return doc.body.innerHTML;
}

export function Markdown({ content, search }: { content: string; search?: string }) {
  const html = useMemo(() => {
    const preprocessed = preprocessMarkdown(content ?? '');
    const raw = marked.parse(preprocessed, { renderer }) as string;
    // SECURITY: model output is untrusted — sanitize before injecting as HTML.
    const sanitized = DOMPurify.sanitize(raw, { ADD_ATTR: ['data-copy'] });
    if (search && search.trim()) {
      return highlightHtml(sanitized, search);
    }
    return sanitized;
  }, [content, search]);

  function onClick(e: MouseEvent) {
    const t = (e.target as HTMLElement)?.closest('.copy-button') as HTMLElement | null;
    if (t?.dataset.copy) {
      const text = decodeURIComponent(t.dataset.copy);
      const prev = t.textContent;
      const mark = (ok: boolean) => {
        t.textContent = ok ? 'Copied' : 'Failed';
        setTimeout(() => {
          t.textContent = prev;
        }, 1200);
      };
      void (async () => {
        try {
          if (navigator.clipboard?.writeText) {
            await navigator.clipboard.writeText(text);
            mark(true);
            return;
          }
        } catch {
          /* fall through */
        }
        try {
          const ta = document.createElement('textarea');
          ta.value = text;
          ta.setAttribute('readonly', '');
          ta.style.position = 'fixed';
          ta.style.left = '-9999px';
          document.body.appendChild(ta);
          ta.select();
          const ok = document.execCommand('copy');
          document.body.removeChild(ta);
          mark(ok);
        } catch {
          mark(false);
        }
      })();
    }
  }

  return (
    <div
      class="markdown-root assistant-message-content"
      onClick={onClick}
      dangerouslySetInnerHTML={{ __html: html }}
    />
  );
}

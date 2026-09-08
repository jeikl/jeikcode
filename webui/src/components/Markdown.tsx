import DOMPurify from 'dompurify';
import { useMemo } from 'preact/hooks';
import { markdownToHtml } from '../lib/markdownRender';

export { preprocessMarkdown, markdownToHtml } from '../lib/markdownRender';

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
    const raw = markdownToHtml(content ?? '');
    // SECURITY: model output is untrusted — sanitize before injecting as HTML.
    const sanitized = DOMPurify.sanitize(raw, {
      ADD_ATTR: ['data-copy', 'class', 'checked', 'disabled', 'type', 'align', 'start', 'colspan', 'rowspan'],
    });
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

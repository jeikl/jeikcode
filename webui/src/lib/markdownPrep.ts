/**
 * 大模型 Markdown 预处理器。
 *
 * marked 的 GFM 表格要求「表头列数 === 分隔行列数」。模型经常写出：
 *   | 目标 | 协议 |
 *   |---|
 * 这种 2 列表头 + 1 列分隔行，整张表会退化成一段 raw `|...|` 文本。
 * 这里在送进 marked 之前把常见瑕疵修掉，并补齐若干聊天场景常用语法。
 */

export interface FenceState {
  marker: '`' | '~';
  length: number;
}

function stripLineBreak(line: string): string {
  return line.replace(/[\r\n]+$/, '');
}

export function fenceOpen(line: string): FenceState | null {
  const raw = stripLineBreak(line);
  let index = 0;
  let indent = 0;
  while (index < raw.length && (raw[index] === ' ' || raw[index] === '\t')) {
    indent += 1;
    index += 1;
  }
  if (indent > 3) return null;

  const marker = raw[index];
  if (marker !== '`' && marker !== '~') return null;

  let markerEnd = index;
  while (markerEnd < raw.length && raw[markerEnd] === marker) markerEnd += 1;
  const length = markerEnd - index;
  if (length < 3) return null;

  const info = raw.slice(markerEnd).trim();
  if (marker === '`' && info.includes('`')) return null;
  return { marker, length };
}

export function fenceClose(line: string, state: FenceState): boolean {
  const raw = stripLineBreak(line);
  let index = 0;
  let indent = 0;
  while (index < raw.length && (raw[index] === ' ' || raw[index] === '\t')) {
    indent += 1;
    index += 1;
  }
  if (indent > 3) return false;

  let markerEnd = index;
  while (markerEnd < raw.length && raw[markerEnd] === state.marker) markerEnd += 1;
  return markerEnd - index >= state.length && raw.slice(markerEnd).trim() === '';
}

export function hasUnescapedPipe(line: string): boolean {
  let escaped = false;
  for (const ch of line) {
    if (ch === '\\') {
      escaped = !escaped;
      continue;
    }
    if (ch === '|' && !escaped) return true;
    escaped = false;
  }
  return false;
}

/** 与 marked.splitCells 对齐：按未转义 `|` 切单元格。 */
export function splitTableCells(line: string): string[] {
  const row = line.replace(/\|/g, (match, offset, str: string) => {
    let escaped = false;
    let curr = offset as number;
    while (--curr >= 0 && str[curr] === '\\') escaped = !escaped;
    return escaped ? '|' : ' |';
  });
  const cells = row.split(/ \|/);
  if (cells.length && !cells[0].trim()) cells.shift();
  if (cells.length && !cells[cells.length - 1].trim()) cells.pop();
  return cells.map((cell) => cell.trim().replace(/\\\|/g, '|'));
}

function isDelimiterCell(cell: string): boolean {
  return /^\s*:?-+:?\s*$/.test(cell) && cell.includes('-');
}

export function isTableDelimiterLine(line: string): boolean {
  const trimmed = line.trim();
  if (!trimmed) return false;
  const cells = splitTableCells(trimmed);
  return cells.length >= 1 && cells.every(isDelimiterCell);
}

function isStrictPipeRow(line: string): boolean {
  const trimmed = line.trim();
  return trimmed.startsWith('|') && trimmed.endsWith('|') && splitTableCells(trimmed).length >= 2;
}

function isMarkdownBlockStart(line: string): boolean {
  const trimmed = line.trim();
  if (!trimmed) return true;
  return (
    /^#{1,6}\s/.test(trimmed) ||
    /^>/.test(trimmed) ||
    /^([-+*]|\d+[.)])\s/.test(trimmed) ||
    /^(```|~~~)/.test(trimmed) ||
    /^([-*_]\s*){3,}$/.test(trimmed)
  );
}

function parseAligns(cells: string[]): Array<'left' | 'right' | 'center' | null> {
  return cells.map((cell) => {
    const raw = cell.trim();
    const left = raw.startsWith(':');
    const right = raw.endsWith(':');
    if (left && right) return 'center';
    if (right) return 'right';
    if (left) return 'left';
    return null;
  });
}

export function buildTableDelimiter(
  columns: number,
  existingCells: string[] = [],
): string {
  const aligns = parseAligns(existingCells);
  const parts: string[] = [];
  for (let i = 0; i < columns; i++) {
    const align = aligns[i];
    if (align === 'left') parts.push(':---');
    else if (align === 'right') parts.push('---:');
    else if (align === 'center') parts.push(':---:');
    else parts.push('---');
  }
  return `| ${parts.join(' | ')} |`;
}

function replaceFullwidthPipes(line: string): string {
  if (!line.includes('｜')) return line;
  let escaped = false;
  let out = '';
  for (const ch of line) {
    if (ch === '\\') {
      escaped = !escaped;
      out += ch;
      continue;
    }
    out += ch === '｜' && !escaped ? '|' : ch;
    escaped = false;
  }
  return out;
}

function fixAtxHeading(line: string): string {
  const headingMatch = line.match(/^(\s*)(#{1,6})([^\s#].*)$/);
  if (!headingMatch) return line;
  return `${headingMatch[1]}${headingMatch[2]} ${headingMatch[3]}`;
}

function escapeHtml(text: string): string {
  return text
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;');
}

function footnoteSlug(id: string): string {
  return id.replace(/[^A-Za-z0-9_-]+/g, '-').replace(/^-+|-+$/g, '') || 'fn';
}

function mapInlineOutsideBackticks(line: string, fn: (chunk: string) => string): string {
  let result = '';
  let index = 0;
  while (index < line.length) {
    if (line[index] !== '`') {
      const next = line.indexOf('`', index);
      const chunk = next === -1 ? line.slice(index) : line.slice(index, next);
      result += fn(chunk);
      index = next === -1 ? line.length : next;
      continue;
    }
    let ticksEnd = index;
    while (ticksEnd < line.length && line[ticksEnd] === '`') ticksEnd += 1;
    const ticks = ticksEnd - index;
    const closer = '`'.repeat(ticks);
    const closeAt = line.indexOf(closer, ticksEnd);
    if (closeAt === -1) {
      result += fn(line.slice(index));
      break;
    }
    result += line.slice(index, closeAt + ticks);
    index = closeAt + ticks;
  }
  return result;
}

function applyInlineMarkdown(source: string, footnoteIds: Map<string, number>): string {
  const lines = source.split('\n');
  let openFence: FenceState | null = null;
  return lines.map((line) => {
    if (openFence) {
      if (fenceClose(line, openFence)) openFence = null;
      return line;
    }
    const opening = fenceOpen(line);
    if (opening) {
      openFence = opening;
      return line;
    }
    return mapInlineOutsideBackticks(line, (chunk) => {
      let text = chunk;
      if (footnoteIds.size) {
        text = text.replace(/\[\^([^\]]+)\]/g, (match, id: string) => {
          const n = footnoteIds.get(id);
          if (!n) return match;
          const slug = footnoteSlug(id);
          return `<sup class="md-footnote-ref"><a href="#md-fn-${slug}" id="md-fnref-${slug}">${n}</a></sup>`;
        });
      }
      // ==高亮==：GFM 没有，但模型经常输出。避开 === 标题线。
      text = text.replace(/==([^=\n]+?)==/g, (_m, inner: string) => {
        return `<mark class="md-highlight">${escapeHtml(inner)}</mark>`;
      });
      return text;
    });
  }).join('\n');
}

export function repairUnclosedFences(source: string): string {
  const text = String(source ?? '');
  let open: FenceState | null = null;
  for (const line of text.split('\n')) {
    if (!open) open = fenceOpen(line);
    else if (fenceClose(line, open)) open = null;
  }
  if (!open) return text;
  const fence = open.marker.repeat(open.length);
  return `${text}${text.endsWith('\n') ? '' : '\n'}${fence}\n`;
}

const GENERIC_LANG = new Set(['', 'text', 'plain', 'plaintext', 'txt', 'output', 'console']);

/** 去掉代码块正文里重复的语言行，例如 ```text 下面第一行又是 text。 */
export function stripLanguageSentinel(code: string, lang: string): string {
  const trailingNl = code.endsWith('\n');
  const text = trailingNl ? code.slice(0, -1) : code;
  const nl = text.indexOf('\n');
  if (nl <= 0) return code;
  const first = text.slice(0, nl).trim().toLowerCase();
  if (!first) return code;
  const langNorm = (lang ?? '').trim().toLowerCase();
  const matchesLang = !!langNorm && first === langNorm;
  const genericLoose = !langNorm && GENERIC_LANG.has(first);
  if (!matchesLang && !genericLoose) return code;
  const rest = text.slice(nl + 1);
  return trailingNl ? `${rest}\n` : rest;
}

export function shouldShowCodeLanguage(lang: string): boolean {
  const normalized = (lang ?? '').trim().toLowerCase();
  return !!normalized && !GENERIC_LANG.has(normalized);
}

/**
 * 鲁棒性 Markdown 预处理器：
 * 1. 保护围栏代码块；
 * 2. 补全 ATX 标题 `#` 后空格；
 * 3. 修复 GFM 表格分隔行列数、缺分隔行、全角竖线；
 * 4. 表格前后补空行，避免后续正文被吞进表格；
 * 5. 未闭合围栏自动补闭合；
 * 6. 脚注、==高亮==。
 */
export function preprocessMarkdown(raw: string): string {
  if (!raw) return '';
  const lines = raw.replace(/\r\n/g, '\n').split('\n');
  const result: string[] = [];
  let inFence: FenceState | null = null;
  let inTable = false;
  const footnotes: { id: string; n: number; def: string }[] = [];
  const footnoteIds = new Map<string, number>();

  const prevBlank = (): boolean => result.length === 0 || result[result.length - 1].trim() === '';

  for (let i = 0; i < lines.length; i++) {
    let line = lines[i];

    if (inFence) {
      result.push(line);
      if (fenceClose(line, inFence)) inFence = null;
      continue;
    }

    const opening = fenceOpen(line);
    if (opening) {
      if (inTable) {
        if (!prevBlank()) result.push('');
        inTable = false;
      }
      result.push(line);
      inFence = opening;
      continue;
    }

    line = replaceFullwidthPipes(line);
    line = fixAtxHeading(line);
    const trimmed = line.trim();

    const footnoteDef = trimmed.match(/^\[\^([^\]]+)\]:\s*(.*)$/);
    if (footnoteDef) {
      const id = footnoteDef[1];
      if (!footnoteIds.has(id)) {
        footnoteIds.set(id, footnotes.length + 1);
        footnotes.push({ id, n: footnotes.length + 1, def: footnoteDef[2] });
      }
      continue;
    }

    if (inTable) {
      if (!trimmed || (isMarkdownBlockStart(line) && !hasUnescapedPipe(line))) {
        inTable = false;
      } else if (hasUnescapedPipe(line) || isTableDelimiterLine(line)) {
        result.push(line);
        const nextLine = i + 1 < lines.length ? lines[i + 1] : null;
        if (nextLine != null) {
          const nextTrimmed = nextLine.trim();
          if (
            nextTrimmed !== '' &&
            !hasUnescapedPipe(nextLine) &&
            !isTableDelimiterLine(nextLine) &&
            !isMarkdownBlockStart(nextLine)
          ) {
            result.push('');
            inTable = false;
          } else if (!nextTrimmed || (isMarkdownBlockStart(nextLine) && !hasUnescapedPipe(nextLine))) {
            inTable = false;
          }
        }
        continue;
      } else {
        if (!prevBlank()) result.push('');
        inTable = false;
      }
    }

    const isHeading = /^\s*#{1,6}\s+/.test(line);
    if (isHeading && result.length > 0) {
      const prevLine = result[result.length - 1].trim();
      if (prevLine !== '' && !prevLine.startsWith('#')) result.push('');
    }

    const nextLine = i + 1 < lines.length ? lines[i + 1] : null;
    const nextIsDelim = nextLine != null && isTableDelimiterLine(nextLine);

    if (hasUnescapedPipe(line) && nextIsDelim) {
      const columns = splitTableCells(line).length;
      const delimRaw = nextLine!.trim();
      // 裸 `---` 是 setext/hr；只有表头已经以 | 开头、或分隔行自带 |/: 时才当成表格。
      if (columns >= 1 && (delimRaw.includes('|') || delimRaw.includes(':') || line.trim().startsWith('|'))) {
        if (!prevBlank()) result.push('');
        result.push(line);
        const delimCells = splitTableCells(nextLine!);
        if (delimCells.length !== columns || !/[:|]/.test(delimRaw)) {
          result.push(buildTableDelimiter(columns, delimCells));
          i += 1;
        }
        inTable = true;
        continue;
      }
    }

    // 模型常省略分隔行：两行都是 | a | b | 形态时补一行 ---。
    if (
      isStrictPipeRow(line) &&
      nextLine != null &&
      isStrictPipeRow(nextLine) &&
      !isTableDelimiterLine(nextLine)
    ) {
      const columns = splitTableCells(line).length;
      if (columns >= 2) {
        if (!prevBlank()) result.push('');
        result.push(line);
        result.push(buildTableDelimiter(columns));
        inTable = true;
        continue;
      }
    }

    result.push(line);
  }

  if (inFence) {
    const fence = inFence.marker.repeat(inFence.length);
    if (result.length === 0 || result[result.length - 1] !== fence) result.push(fence);
  }

  let text = result.join('\n');
  text = applyInlineMarkdown(text, footnoteIds);
  if (footnotes.length) {
    const items = footnotes
      .map((fn) => {
        const slug = footnoteSlug(fn.id);
        return `<li id="md-fn-${slug}">${escapeHtml(fn.def)} <a href="#md-fnref-${slug}" class="md-footnote-back">↩</a></li>`;
      })
      .join('\n');
    text += `\n\n<div class="md-footnotes"><hr>\n<ol>\n${items}\n</ol></div>\n`;
  }
  return text;
}

/** OpenCode-style tool chrome shared by the chat tool rows. */

export type ToolCategory =
  | 'file'
  | 'edit'
  | 'search'
  | 'terminal'
  | 'globe'
  | 'folder'
  | 'skill'
  | 'todo'
  | 'mcp'
  | 'default';

export function toolCategory(name: string): ToolCategory {
  if (name.startsWith('mcp__')) return 'mcp';
  switch (name) {
    case 'read_file':
      return 'file';
    case 'edit_file':
    case 'write_file':
    case 'create_file':
    case 'search_replace':
    case 'parallel_edit_files':
      return 'edit';
    case 'grep':
    case 'glob':
    case 'code_explore':
      return 'search';
    case 'bash':
      return 'terminal';
    case 'web_fetch':
    case 'web_search':
      return 'globe';
    case 'list_directory':
    case 'change_dir':
      return 'folder';
    case 'use_skill':
      return 'skill';
    case 'todo':
    case 'todowrite':
      return 'todo';
    default:
      return 'default';
  }
}

/** OpenCode inline-tool glyphs: `$` bash, `←` edit, `→` read, `✱` search, `◈` web. */
export function toolGlyph(name: string): string {
  switch (toolCategory(name)) {
    case 'terminal':
      return '$';
    case 'edit':
      return '←';
    case 'file':
      return '→';
    case 'search':
      return '✱';
    case 'globe':
      return '◈';
    default:
      return '⚙';
  }
}

export function jsonArgString(argsJson: string, key: string): string {
  try {
    const parsed = JSON.parse(argsJson) as unknown;
    if (parsed === null || typeof parsed !== 'object') return '';
    const v = (parsed as Record<string, unknown>)[key];
    return typeof v === 'string' ? v : '';
  } catch {
    return '';
  }
}

export type DiffPreviewLine = {
  kind: 'add' | 'del' | 'ctx' | 'meta';
  text: string;
  oldLine?: number;
  newLine?: number;
};

/** Tools whose payload is actually a code change (edit/write/git). Bullet lists
 *  in `code_explore` / skills / grep must never go through the diff highlighter. */
export function toolRendersAsDiff(name: string): boolean {
  switch (name) {
    case 'edit_file':
    case 'write_file':
    case 'create_file':
    case 'search_replace':
    case 'parallel_edit_files':
    case 'bash':
      return true;
    default:
      return false;
  }
}

/** True only for a real unified diff (`diff --git` or `@@ -n,n +n,n @@`).
 *  A leading `-` bullet (`- F3 'memory/…'`) is not a deletion. */
export function looksLikeUnifiedDiff(text: string): boolean {
  const lines = text.replace(/\r\n/g, '\n').split('\n');
  const n = Math.min(lines.length, 80);
  for (let i = 0; i < n; i++) {
    const line = lines[i]!;
    if (line.startsWith('diff --git ') || /^@@ -\d+/.test(line)) return true;
  }
  return false;
}

function parseHunkStarts(header: string): { oldStart: number; newStart: number } | null {
  let oldStart: number | undefined;
  let newStart: number | undefined;
  for (const tok of header.split(/\s+/)) {
    if (tok.startsWith('-')) {
      const n = Number.parseInt(tok.slice(1).split(',')[0] ?? '', 10);
      if (Number.isFinite(n)) oldStart = n;
    } else if (tok.startsWith('+')) {
      const n = Number.parseInt(tok.slice(1).split(',')[0] ?? '', 10);
      if (Number.isFinite(n)) newStart = n;
    }
  }
  return oldStart != null && newStart != null ? { oldStart, newStart } : null;
}

/** Build a synthetic unified-diff preview from edit args when tool output
 *  has no `@@` hunk (failed edits, write_file stats, etc.). */
export function buildEditArgsDiff(oldStr: string, newStr: string): DiffPreviewLine[] {
  const oldLines = oldStr.replace(/\r\n/g, '\n').split('\n');
  const newLines = newStr.replace(/\r\n/g, '\n').split('\n');
  if (oldLines.length === 0 && newLines.length === 0) return [];
  const header = `@@ -1,${Math.max(oldLines.length, 1)} +1,${Math.max(newLines.length, 1)} @@`;
  const body: string[] = [header];
  for (const line of oldLines) body.push(`-${line}`);
  for (const line of newLines) body.push(`+${line}`);
  return parseDiffPreview(body.join('\n'));
}

/** Format synthetic diff as copyable unified-diff text. */
export function formatEditArgsDiffRaw(oldStr: string, newStr: string): string {
  const oldLines = oldStr.replace(/\r\n/g, '\n').split('\n');
  const newLines = newStr.replace(/\r\n/g, '\n').split('\n');
  const header = `@@ -1,${Math.max(oldLines.length, 1)} +1,${Math.max(newLines.length, 1)} @@`;
  const body: string[] = [header];
  for (const line of oldLines) body.push(`-${line}`);
  for (const line of newLines) body.push(`+${line}`);
  return body.join('\n');
}

export function normalizeToolOutputText(raw: string): string {
  return raw
    .replace(/\\r\\n/g, '\n')
    .replace(/\\n/g, '\n')
    .replace(/\\t/g, '\t')
    .replace(/\\"/g, '"');
}

/** Resolve diff lines: prefer real unified diff in output, else old/new args. */
export function resolveToolDiffPreview(
  name: string,
  output: string | undefined,
  args: string | undefined,
): { lines: DiffPreviewLine[]; raw: string; source: 'output' | 'args' } | null {
  if (!toolRendersAsDiff(name)) return null;
  const normalized = output ? normalizeToolOutputText(output) : '';
  if (normalized && looksLikeUnifiedDiff(normalized)) {
    const lines = parseDiffPreview(normalized);
    if (lines.some((l) => l.kind === 'add' || l.kind === 'del')) {
      return { lines, raw: normalized, source: 'output' };
    }
  }
  if (!args) return null;
  const oldStr = jsonArgString(args, 'old_string');
  const newStr = jsonArgString(args, 'new_string');
  if (!oldStr && !newStr) return null;
  const lines = buildEditArgsDiff(oldStr, newStr);
  if (!lines.some((l) => l.kind === 'add' || l.kind === 'del')) return null;
  return { lines, raw: formatEditArgsDiffRaw(oldStr, newStr), source: 'args' };
}

export type ToolDiffStats = {
  additions: number;
  deletions: number;
};

/** Compute additions and deletions (+N -M) for a tool call. */
export function computeToolDiffStats(
  name: string,
  output?: string,
  args?: string,
): ToolDiffStats | null {
  const diff = resolveToolDiffPreview(name, output, args);
  if (diff && diff.lines.length > 0) {
    let additions = 0;
    let deletions = 0;
    for (const line of diff.lines) {
      if (line.kind === 'add') additions++;
      else if (line.kind === 'del') deletions++;
    }
    if (additions > 0 || deletions > 0) {
      return { additions, deletions };
    }
  }

  if (output) {
    const addMatch = /(\d+)\s+(?:insertions?|additions?|\(\+\))/i.exec(output);
    const delMatch = /(\d+)\s+(?:deletions?|\(-\))/i.exec(output);
    if (addMatch || delMatch) {
      const additions = addMatch ? Number.parseInt(addMatch[1]!, 10) : 0;
      const deletions = delMatch ? Number.parseInt(delMatch[1]!, 10) : 0;
      if (additions > 0 || deletions > 0) {
        return { additions, deletions };
      }
    }
  }

  if (name === 'write_file' || name === 'create_file') {
    if (args) {
      const content = jsonArgString(args, 'content') || jsonArgString(args, 'code_content');
      if (content) {
        const lineCount = content.replace(/\r\n/g, '\n').split('\n').length;
        return { additions: lineCount, deletions: 0 };
      }
    }
  }

  return null;
}

export type TurnDiffSummary = {
  fileCount: number;
  additions: number;
  deletions: number;
  toolCount: number;
};

/** Collect aggregated diff metrics (total files changed, +N -M) across turn message parts. */
export function collectTurnDiffSummary(
  parts: Array<{ kind: string; tool?: { name: string; output?: string; args?: string; id?: string } }>,
): TurnDiffSummary | null {
  const toolParts = parts.filter((p) => p.kind === 'tool' && p.tool);
  if (toolParts.length === 0) return null;

  let totalAdditions = 0;
  let totalDeletions = 0;
  let hasDiff = false;
  const editedFiles = new Set<string>();

  for (const p of toolParts) {
    const tool = p.tool!;
    const stats = computeToolDiffStats(tool.name, tool.output, tool.args);
    if (stats && (stats.additions > 0 || stats.deletions > 0)) {
      totalAdditions += stats.additions;
      totalDeletions += stats.deletions;
      hasDiff = true;
    }
    if (toolRendersAsDiff(tool.name) || toolCategory(tool.name) === 'edit') {
      const filePath = tool.args
        ? jsonArgString(tool.args, 'file_path') || jsonArgString(tool.args, 'path')
        : '';
      if (filePath) {
        editedFiles.add(filePath);
      } else if (tool.id) {
        editedFiles.add(tool.id);
      }
    }
  }

  const fileCount = editedFiles.size > 0 ? editedFiles.size : toolParts.length;
  if (!hasDiff && editedFiles.size === 0) return null;

  return {
    fileCount,
    additions: totalAdditions,
    deletions: totalDeletions,
    toolCount: toolParts.length,
  };
}

/** Best-effort unified-diff preview with per-line numbers from `@@` hunks.
 *  `+/-` lines are only colored after a real diff header — never on first sight. */
export function parseDiffPreview(output: string, maxLines = 2000): DiffPreviewLine[] {
  const raw = output.replace(/\r\n/g, '\n').split('\n');
  const out: DiffPreviewLine[] = [];
  let sawDiff = false;
  let oldLn = 0;
  let newLn = 0;
  let inHunk = false;
  for (const line of raw) {
    if (out.length >= maxLines) break;
    if (
      line.startsWith('diff --git') ||
      line.startsWith('index ') ||
      line.startsWith('--- ') ||
      line.startsWith('+++ ') ||
      line.startsWith('@@')
    ) {
      sawDiff = true;
      if (line.startsWith('@@')) {
        const starts = parseHunkStarts(line);
        if (starts) {
          oldLn = starts.oldStart;
          newLn = starts.newStart;
          inHunk = true;
        }
      } else {
        inHunk = false;
      }
      out.push({ kind: 'meta', text: line });
      continue;
    }
    if (!sawDiff) continue;
    if (line.startsWith('+') && !line.startsWith('+++ ')) {
      out.push({
        kind: 'add',
        text: line,
        newLine: inHunk ? newLn : undefined,
      });
      if (inHunk) newLn += 1;
      continue;
    }
    if (line.startsWith('-') && !line.startsWith('--- ')) {
      out.push({
        kind: 'del',
        text: line,
        oldLine: inHunk ? oldLn : undefined,
      });
      if (inHunk) oldLn += 1;
      continue;
    }
    const isCtx = line.startsWith(' ') || line === '';
    out.push({
      kind: 'ctx',
      text: line,
      oldLine: inHunk && isCtx ? oldLn : undefined,
      newLine: inHunk && isCtx ? newLn : undefined,
    });
    if (inHunk && isCtx) {
      oldLn += 1;
      newLn += 1;
    }
  }
  return sawDiff ? out : [];
}

export type StructuredField = {
  key: string;
  value: string;
  multiline: boolean;
};

function tryParseJson(text: string): unknown | undefined {
  const trimmed = text.trim();
  if (!trimmed) return undefined;
  const starts = trimmed[0];
  if (starts !== '{' && starts !== '[' && starts !== '"') return undefined;
  try {
    return JSON.parse(trimmed) as unknown;
  } catch {
    return undefined;
  }
}

function prettyUnknown(value: unknown): string {
  if (typeof value === 'string') {
    const nested = tryParseJson(value);
    if (nested !== undefined && typeof nested !== 'string') {
      return JSON.stringify(nested, null, 2);
    }
    return value;
  }
  if (value === null || value === undefined) return String(value);
  if (typeof value === 'object') return JSON.stringify(value, null, 2);
  return String(value);
}

/** Turn a raw tool-args JSON blob into readable key/value fields.
 * Nested JSON strings and `\n` escapes become real formatted text. */
export function structuredToolFields(raw: string): StructuredField[] | null {
  const parsed = tryParseJson(raw);
  if (parsed === undefined || parsed === null || typeof parsed !== 'object' || Array.isArray(parsed)) {
    return null;
  }
  return Object.entries(parsed as Record<string, unknown>).map(([key, val]) => {
    const value = prettyUnknown(val);
    return { key, value, multiline: value.includes('\n') };
  });
}

/** Pretty-print tool args/output: unescape JSON, expand nested strings. */
export function formatToolPayload(raw: string): string {
  if (!raw) return raw;
  const pretty = prettyToolText(raw);
  return pretty.text;
}

/** Copyable code-block body: pretty JSON when the payload parses, else unescaped text. */
export function prettyToolText(raw: string): { text: string; lang: 'json' | 'text' } {
  if (!raw) return { text: '', lang: 'text' };
  const parsed = tryParseJson(raw);
  if (parsed !== undefined) {
    return { text: JSON.stringify(parsed, null, 2), lang: 'json' };
  }
  return {
    text: raw.replace(/\\r\\n/g, '\n').replace(/\\n/g, '\n').replace(/\\t/g, '\t').replace(/\\"/g, '"'),
    lang: 'text',
  };
}

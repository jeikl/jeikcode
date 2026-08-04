/**
 * Path helpers for webui display.
 *
 * Windows `canonicalize` / `std::fs::canonicalize` returns extended-length
 * paths like `\\?\E:\desktop`. Showing that raw string in the cwd chip is
 * noisy and confuses users — strip the prefix for display only.
 */

/** Strip Windows `\\?\` / `//?/` extended-length prefix (and UNC `\\?\UNC\`). */
export function stripExtendedPathPrefix(path: string): string {
  if (!path) return '';
  // UNC: \\?\UNC\server\share → \\server\share
  if (/^\\\\\?\\UNC\\/i.test(path)) {
    return '\\\\' + path.slice('\\\\?\\UNC\\'.length);
  }
  // Drive: \\?\E:\foo or //?/E:/foo (some layers normalize slashes)
  if (/^\\\\\?\\/i.test(path)) {
    return path.slice(4);
  }
  if (/^\/\/\?\//i.test(path)) {
    return path.slice(4);
  }
  return path;
}

/** Collapse home prefixes to `~` for readability. */
export function collapseHomePath(path: string): string {
  const p = stripExtendedPathPrefix(path);
  if (!p) return '';
  return p
    .replace(/^\/(?:Users|home)\/[^/]+/, '~')
    .replace(/^[A-Za-z]:[/\\]Users[/\\][^/\\]+/i, '~');
}

/** Short display path for the input cwd chip / breadcrumbs. */
export function displayPath(path: string): string {
  return collapseHomePath(path);
}

/** Last path segment (project folder name), separator-agnostic. */
export function pathBasename(path: string): string {
  const p = stripExtendedPathPrefix(path).replace(/[/\\]+$/, '');
  if (!p) return '';
  const parts = p.split(/[/\\]/);
  return parts[parts.length - 1] || p;
}

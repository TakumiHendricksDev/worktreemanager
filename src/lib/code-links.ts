/**
 * File references in an agent's reply, turned into places the Code tab can open.
 *
 * An agent names files the way a developer would: `src/app.py`, `src/app.py:42`,
 * `src/app.py:42-58`, `app.py#L42`, `./src/app.py`, or an absolute path inside the worktree. This
 * reads those out of an inline code span or a diff header, and — the important half — keeps only
 * the ones that are real files in the worktree, so `foo.bar` in prose or a path from some other
 * project never becomes a link that opens nothing.
 *
 * Pure, over a file list, because there is no JS test runner and the cases are the part worth
 * getting right.
 */

/** A file and, when the reference named one, a line or a range of lines. */
export interface CodeRef {
  path: string;
  line: number | null;
  end: number | null;
}

/** What a reference is resolved against: the worktree's files and where the worktree is. */
export interface FileIndex {
  files: ReadonlySet<string>;
  /** Absolute, without a trailing slash. */
  root: string;
  /** Files by their name alone, for a reference that gives only the name. */
  byName: ReadonlyMap<string, string[]>;
}

export function fileIndex(files: readonly string[], root: string): FileIndex {
  const byName = new Map<string, string[]>();
  for (const path of files) {
    const name = path.slice(path.lastIndexOf('/') + 1);
    const list = byName.get(name);
    if (list) list.push(path);
    else byName.set(name, [path]);
  }
  return { files: new Set(files), root: root.replace(/\/+$/, ''), byName };
}

/** `path`, then `:12`, `:12-18`, `:12:5` or `#L12`, `#L12-L18`. */
const REFERENCE = /^(.+?)(?::(\d+)(?:[-–](\d+)|:\d+)?|#L(\d+)(?:-L?(\d+))?)?$/;

/**
 * Read a reference out of text, or null when it does not name a file in the worktree.
 *
 * A bare name — `views.py:12` — is accepted only when exactly one file in the worktree has that
 * name, and a partial path — `events/views.py` — only when exactly one file ends with it: a guess
 * between two files would open the wrong one as often as the right one.
 */
export function resolveRef(text: string, index: FileIndex): CodeRef | null {
  const trimmed = text.trim();
  if (trimmed === '' || trimmed.length > 400 || /\s/.test(trimmed)) return null;
  const match = REFERENCE.exec(trimmed);
  if (!match) return null;
  let path = match[1] ?? '';
  const line = Number(match[2] ?? match[4] ?? NaN);
  const end = Number(match[3] ?? match[5] ?? NaN);

  if (path.startsWith(`${index.root}/`)) path = path.slice(index.root.length + 1);
  path = path.replace(/^\.\//, '');
  const found = locate(path, index);
  if (!found) return null;
  return {
    path: found,
    line: Number.isFinite(line) && line > 0 ? line : null,
    end: Number.isFinite(end) && end >= line ? end : null,
  };
}

function locate(path: string, index: FileIndex): string | null {
  if (path === '' || path.startsWith('/') || path.includes('..')) return null;
  if (index.files.has(path)) return path;
  const name = path.slice(path.lastIndexOf('/') + 1);
  const candidates = (index.byName.get(name) ?? []).filter(
    (file) => file === path || file.endsWith(`/${path}`),
  );
  return candidates.length === 1 ? (candidates[0] ?? null) : null;
}

/** The file a unified diff's `+++ b/…` or `--- a/…` header names, or null. */
export function diffHeaderPath(line: string): string | null {
  const match = /^(?:\+\+\+|---) (?:[ab]\/)?(.+?)\s*$/.exec(line);
  const path = match?.[1];
  return path && path !== '/dev/null' ? path : null;
}

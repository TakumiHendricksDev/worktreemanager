/**
 * Comments on code, turned into what an agent is told — pure, so it can be read and reasoned about
 * without a test runner, which this frontend does not have.
 *
 * # Drafted, never sent
 *
 * Everything here ends in the agent's composer, not in a turn. The user reads what the agent is
 * about to be told, adds the question, and presses Enter — which is the one direct action that
 * sends their code to a model provider, the same rule browser comments and the Database tab's Send
 * to agent keep.
 */

import { fenceFor } from './code-languages';
import type { CodeComment } from './ipc/types';

/** Past this many lines an excerpt is named rather than quoted; the agent can read the file. */
export const MAX_QUOTED_LINES = 80;

/** Lines `start..=end` (1-based) of a text, joined as they are in the file. */
export function excerptOf(text: string, start: number, end: number): string {
  return text
    .split('\n')
    .slice(start - 1, end)
    .join('\n');
}

/** The lines a comment is on no longer say what they said when it was written. */
export function isOutdated(comment: CodeComment, text: string): boolean {
  return excerptOf(text, comment.start, comment.end) !== comment.excerpt;
}

/** `path:12` or `path:12-18`, the form an agent can open. */
export function reference(path: string, start: number, end: number): string {
  return start === end ? `${path}:${start}` : `${path}:${start}-${end}`;
}

/**
 * A fence long enough that nothing inside the excerpt can close it: one backtick more than the
 * longest run of them in the text, and never fewer than three.
 */
function fence(excerpt: string): string {
  const longest = Math.max(0, ...[...excerpt.matchAll(/`+/g)].map((m) => m[0].length));
  return '`'.repeat(Math.max(3, longest + 1));
}

/** One quoted range, or its reference alone when it is too long to quote. */
function quote(path: string, start: number, end: number, excerpt: string): string[] {
  if (end - start + 1 > MAX_QUOTED_LINES) {
    return [
      `(${end - start + 1} lines — too many to quote here; read them from the file.)`,
    ];
  }
  const f = fence(excerpt);
  return [`${f}${fenceFor(path)}`, excerpt, f];
}

/**
 * The message for a batch of review comments.
 *
 * Numbered, each with its reference, the lines it is about as they were when it was written, and
 * the comment. An outdated one says so, because the agent will open a file that no longer matches
 * the quote.
 */
export function reviewMessage(
  comments: readonly CodeComment[],
  outdated: (comment: CodeComment) => boolean,
): string {
  const lines = [
    comments.length === 1
      ? 'A review comment on this worktree:'
      : `${comments.length} review comments on this worktree:`,
    '',
  ];
  comments.forEach((comment, index) => {
    lines.push(`${index + 1}. \`${reference(comment.path, comment.start, comment.end)}\``);
    if (outdated(comment)) {
      lines.push('   (The file has changed since; this is what those lines said then.)');
    }
    lines.push(...quote(comment.path, comment.start, comment.end, comment.excerpt));
    lines.push(comment.text, '');
  });
  return lines.join('\n');
}

/** The message for a quick question about a selection: the lines, then room to ask. */
export function askMessage(
  path: string,
  start: number,
  end: number,
  excerpt: string,
): string {
  return [
    `About \`${reference(path, start, end)}\`:`,
    ...quote(path, start, end, excerpt),
    '',
    '',
  ].join('\n');
}

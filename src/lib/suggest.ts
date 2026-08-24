/**
 * What the composer should be offering, given a draft and a caret.
 *
 * Pure functions, no DOM, no state — the same shape as `markdown.ts` and for the same reason:
 * **there is no JS test runner in this project.** Logic that can be reached only by typing into a
 * live textarea cannot be checked at all, so the decisions worth getting right are pulled out here
 * where a probe can call them directly with a hundred inputs.
 *
 * Two triggers, one mechanism:
 *
 *   * `@` opens the worktree's files, anywhere a word can start;
 *   * `/` opens the session's skills, and **only at position 0**.
 *
 * # Why `/` is the strict one
 *
 * Because a slash is a path separator and an `@` is not. `src/lib/` is four characters of ordinary
 * prose in this app's domain, and a menu that opened on each of them would fire constantly while
 * someone typed a path by hand. Both CLIs also only honour a slash command as the whole first token
 * of a message, so restricting it to the start is not a UI compromise — it is where the feature
 * actually works.
 */

/**
 * How many rows the strip will offer.
 *
 * Was 20, on the grounds that more is a list nobody reads to the end of. That is true of a list you
 * *scroll*, and the strip does scroll (`_suggest.scss` caps its height and lets it overflow), so 20
 * was not a reading limit — it was a **reachability** limit, and it was cutting a repository's own
 * skills out of the menu entirely: with ~110 built-in commands in the catalogue, a bare `/` never
 * got near them. The order is what makes a list readable; this only decides what is reachable at
 * all, and it should be generous.
 */
const LIMIT = 50;

/**
 * The longest query worth matching.
 *
 * A guard, not a preference: the scan below is O(query × candidates), so a paste of a whole
 * paragraph after an `@` would otherwise be tens of millions of character comparisons on a
 * keystroke. Past this length nothing is going to match anyway.
 */
const MAX_QUERY = 64;

export type Trigger = 'file' | 'skill';

/** An open typeahead: what kind, what has been typed, and the span it will replace. */
export interface Query {
  kind: Trigger;
  /** What follows the trigger character, lowercased for matching. */
  text: string;
  /** Index of the trigger character itself — the start of the range an accept overwrites. */
  start: number;
  /** One past the caret — the end of that range. */
  end: number;
}

/** One offered row. `detail` is the second column, and may be empty. */
export interface Suggestion {
  /** What gets inserted, without the trigger character. */
  value: string;
  /** The first column — a filename, a skill name. */
  label: string;
  /** The second column — a directory, a description. Empty renders as one column. */
  detail: string;
}

/**
 * Read the caret's surroundings for an open trigger, or `null`.
 *
 * Scans backwards from the caret to the nearest whitespace. That bound is what makes the whole
 * thing cheap and is also the dismissal rule: typing a space closes the strip, because the token
 * the trigger started is over.
 */
export function queryAt(draft: string, caret: number): Query | null {
  // Everything before the caret only. A trigger to the *right* of the caret belongs to a token the
  // user has already moved past, and completing it would rewrite text they are not looking at.
  const head = draft.slice(0, caret);

  let start = head.length;
  while (start > 0) {
    const char = head[start - 1] ?? '';
    // Newline included deliberately: a `/` at the start of a *line* is not a slash command, and a
    // multi-line draft would otherwise reopen the skill list on every paragraph.
    if (char === ' ' || char === '\t' || char === '\n') break;
    start -= 1;
  }

  const token = head.slice(start);
  const kind: Trigger | null = token.startsWith('@')
    ? 'file'
    : token.startsWith('/') && start === 0
      ? 'skill'
      : null;
  if (kind === null) return null;

  const text = token.slice(1);
  if (text.length > MAX_QUERY) return null;
  return { kind, text: text.toLowerCase(), start, end: caret };
}

/**
 * Rank paths against a query.
 *
 * Subsequence matching, not substring: `slsv` finds `src/lib/state/sessions.svelte.ts`, which is
 * the behaviour that makes a typeahead over a few thousand paths usable at all. An empty query
 * offers the first `LIMIT` paths rather than nothing, so `@` alone is a browsable list.
 */
export function matchFiles(paths: readonly string[], query: string): Suggestion[] {
  const scored = rank(paths, query, (path) => path);
  return scored.map((path) => {
    const cut = path.lastIndexOf('/');
    return {
      value: path,
      // The filename leads, because that is what was typed and what identifies the file. A list of
      // full paths left-aligned puts the shared prefix — `src/lib/components/` for a dozen rows —
      // where the eye lands, and buries the one word that differs.
      label: cut === -1 ? path : path.slice(cut + 1),
      detail: cut === -1 ? '' : path.slice(0, cut),
    };
  });
}

/** Rank skills against a query. Matched on the name only — a description is context, not a key. */
export function matchSkills(
  skills: readonly { name: string; description: string | null }[],
  query: string,
): Suggestion[] {
  return rank(skills, query, (skill) => skill.name).map((skill) => ({
    value: skill.name,
    label: skill.name,
    detail: skill.description ?? '',
  }));
}

/**
 * The shared scorer: keep what matches, best first, capped.
 *
 * The score is deliberately crude — three signals, no tuning pass — because a fuzzy finder that
 * cannot be explained is one nobody can fix when it puts the wrong row first:
 *
 *   1. a prefix match beats everything, so typing a name exactly puts it at the top;
 *   2. then a contiguous substring, so `sessions` prefers `sessions.svelte.ts` over a path whose
 *      letters merely appear in order;
 *   3. then how tightly the subsequence packed, so the shortest span wins.
 *
 * Ties break on length, shortest first: given `App.svelte` and `AppHeader.svelte`, the one that is
 * closest to what was typed is the more likely target.
 */
function rank<T>(items: readonly T[], query: string, key: (item: T) => string): T[] {
  // Nothing typed yet, so there is nothing to score by and the caller's order stands. That order is
  // load-bearing rather than incidental: `commandsFor` puts the session's own discovered skills
  // ahead of the built-in catalogue precisely because this line is what a bare `/` shows.
  if (query === '') return items.slice(0, LIMIT);

  const hits: { item: T; score: number; length: number }[] = [];
  for (const item of items) {
    const text = key(item);
    const score = score_(text.toLowerCase(), query);
    if (score !== null) hits.push({ item, score, length: text.length });
  }

  hits.sort((a, b) => b.score - a.score || a.length - b.length);
  return hits.slice(0, LIMIT).map((hit) => hit.item);
}

/** `null` when the query is not a subsequence of the text. Higher is better. */
function score_(text: string, query: string): number | null {
  if (text.startsWith(query)) return 3000;
  if (text.includes(query)) return 2000;

  // Walk both once. `first`/`last` bracket where the query's characters landed, and a tighter
  // bracket is a better match — `mdts` scoring `markdown.ts` above `model-directory-tests.ts`.
  let at = -1;
  let first = -1;
  for (const char of query) {
    at = text.indexOf(char, at + 1);
    if (at === -1) return null;
    if (first === -1) first = at;
  }
  const span = at - first + 1;
  // Bounded below at 1 so a very loose match still outranks no match, and always below the two
  // tiers above — a subsequence must never beat a substring.
  return Math.max(1, 1000 - span);
}

/**
 * Put an accepted suggestion into the draft, and say where the caret goes.
 *
 * The trailing space is the important part: it is what closes the strip, because `queryAt` stops at
 * whitespace. Without it, accepting a suggestion leaves the menu open over the thing it just
 * inserted, and the next keystroke reopens the search.
 *
 * It is added only when there is not one already. Completing mid-sentence — `look at @src/li| and
 * fix it` — otherwise produced a double space before `and`, because the tail the insert is spliced
 * in front of begins with one. Measured, not reasoned about; the first version of this shipped the
 * double.
 *
 * The caret lands after the inserted name either way, before any space that was already there, so
 * typing continues where the eye is.
 */
export function accept(
  draft: string,
  query: Query,
  value: string,
): { draft: string; caret: number } {
  const trigger = query.kind === 'file' ? '@' : '/';
  const tail = draft.slice(query.end);
  const spaced = tail.startsWith(' ') || tail.startsWith('\t') || tail.startsWith('\n');
  const inserted = `${trigger}${value}${spaced ? '' : ' '}`;
  return {
    draft: draft.slice(0, query.start) + inserted + tail,
    caret: query.start + inserted.length,
  };
}

/**
 * A dropped or picked path, as it should appear in a message.
 *
 * Relative to the worktree where it is inside one, absolute where it is not. Relative because the
 * agent's cwd *is* the worktree, so `src/lib/foo.ts` is both shorter and unambiguous — and because
 * an absolute path puts the user's home directory into every prompt for no benefit.
 *
 * The boundary check is on a trailing separator, not a bare `startsWith`: without it a sibling
 * worktree named `feature-auth-2` would be treated as living inside `feature-auth`, and the path
 * handed to the agent would be a relative one that resolves nowhere.
 */
export function relativise(path: string, worktree: string): string {
  const root = worktree.endsWith('/') ? worktree : `${worktree}/`;
  return path.startsWith(root) ? path.slice(root.length) : path;
}

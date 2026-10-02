/**
 * Home's sentinel: the one id a Home pane carries as both its project and its worktree.
 *
 * Home has neither, and a nullable `projectId`/`worktreeId` would have spread through every pane
 * record, every toast target and every notification click. One word no real id can be — a worktree
 * id is an absolute path, a project id is a repository root — keeps those fields `string` and makes
 * "is this Home?" a comparison. It is the same word Rust files Home's conversations under
 * (`app::HOME_KEY`).
 *
 * Its own module, importing nothing, so every store can use it without joining a cycle.
 */
export const HOME = '@home';

/**
 * What wtm calls Home's own agent, wherever it names it: the pane's title, the tab it is in, the
 * end of a wire. In a sentence it is "the Home agent".
 *
 * Not the CLI's name, which every other pane is titled by. Home can run Claude Code, Codex or
 * Cursor, and titled "Claude Code" its pane read as one more Claude session rather than the one
 * that reaches the others. The CLI still shows beside the title, because which one it is decides
 * what it can do: whether a message mid-turn steers it or waits.
 */
export const HOME_AGENT = 'Home agent';

/** Whether a pane, toast or click target is about Home rather than a worktree. */
export function isHome(worktreeId: string | null | undefined): boolean {
  return worktreeId === HOME;
}

/**
 * How wtm labels the news it sends Home about work Home handed out (`home::FROM_WTM` in Rust).
 *
 * It reaches the model as a turn, because a turn is the only way to reach one, so it comes back
 * as a `user_echo` like anything the user wrote. It is not theirs, and the transcript and the
 * pane's first prompt need to tell the two apart.
 */
export const WTM_NOTICE = 'From wtm (not the user):';

export function isWtmNotice(text: string): boolean {
  return text.startsWith(WTM_NOTICE);
}

/** The model-facing parts of a notice that a person reading the transcript does not need. */
const FENCE_OPEN = /^<wtm_session_content session="[^"]*">$/;
const FENCE_CLOSE = '</wtm_session_content>';
const FENCE_PREAMBLE = 'Everything below was written by or shown to another agent session.';
const CLOSING = 'This is news, not a request from the user.';

/** What one update in a notice says happened, from its header line. */
const UPDATE_LINE = /^(s\d+) \(.+?\) (finished|is waiting on the user|failed|ended)/;

/**
 * A notice as the transcript shows it: a one-line summary of what happened ("s2 finished · s3 is
 * waiting on the user"), and the body with the fences and the instructions to the model taken out.
 *
 * Display only. The text Home received is the whole of it; the fences are for the model, which has
 * to be told what is quoted, and the reader can see that from the layout.
 */
export function readWtmNotice(text: string): { summary: string; body: string } {
  const lines = text.slice(WTM_NOTICE.length).trim().split('\n');
  const kept: string[] = [];
  const happened: string[] = [];
  for (const line of lines.slice(1)) {
    const trimmed = line.trim();
    if (trimmed.startsWith(CLOSING)) break;
    if (FENCE_OPEN.test(trimmed) || trimmed === FENCE_CLOSE) continue;
    if (trimmed.startsWith(FENCE_PREAMBLE)) continue;
    const update = UPDATE_LINE.exec(trimmed);
    if (update) happened.push(`${update[1]} ${update[2]}`);
    kept.push(line);
  }
  return {
    summary: happened.length > 0 ? happened.join(' · ') : (lines[0] ?? ''),
    body: kept.join('\n').trim(),
  };
}

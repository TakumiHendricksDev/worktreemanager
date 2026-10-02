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

/** Whether a pane, toast or click target is about Home rather than a worktree. */
export function isHome(worktreeId: string | null | undefined): boolean {
  return worktreeId === HOME;
}

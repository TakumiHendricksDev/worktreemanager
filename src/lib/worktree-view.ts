/**
 * The worktree's own views, switched from the bar: its sessions, its databases, its code.
 *
 * A name for the union rather than three booleans, because exactly one is ever showing, and the
 * bar, `Detail` and `App` all have to agree on the spelling.
 */
export type WorktreeView = 'sessions' | 'database' | 'code';

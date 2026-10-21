import type { Finding } from '../knowledge';

/**
 * The IPC data contract.
 *
 * These mirror the `Serialize` structs in `src-tauri/src/view.rs`. They are hand-written
 * rather than generated: a code generator for this boundary is one more build step and
 * one more dependency, and the surface is small enough that a Rust-side test
 * (`view::tests::contract_shape_is_camel_case`) snapshots the serialized key names to
 * catch drift.
 *
 * If you add a field in `view.rs`, add it here.
 */

export type FieldKind =
  'text' | 'multiline' | 'number' | 'bool' | 'select' | 'multiselect' | 'path';

export type PreflightSeverity = 'error' | 'warn' | 'info';

export interface TrustPrompt {
  path: string;
  /** Every argv the config would run, verbatim. */
  commands: string[][];
  /** Database targets the config could connect to. Credentials are never included. */
  databases: DatabaseTrustDeclaration[];
  contentHash: string;
}

export interface DatabaseTrustDeclaration {
  id: string;
  engine: string;
  target: string;
  passwordConfigured: boolean;
}

export interface Project {
  id: string;
  name: string;
  root: string;
  /** False when the config failed to load or needs trust approval. */
  usable: boolean;
  problem: string | null;
  trust: TrustPrompt | null;
}

/**
 * The new project list, plus which entry was registered.
 *
 * `id` is here because this side cannot work it out: registration accepts any path inside a
 * repository and resolves it to the toplevel, so `~/Sites/foo` and `…/foo/src` both come back
 * as `/absolute/Sites/foo`. Matching the typed string against the roots is what used to fail.
 */
export interface Registered {
  id: string;
  projects: Project[];
}

export interface Badge {
  label: string;
  value: string;
}

export interface Link {
  label: string;
  url: string;
}

export interface TableRow {
  label: string;
  value: string;
  /** True when the value came from a defaults source, not the worktree's own file. */
  inherited: boolean;
  url: string | null;
}

export interface Worktree {
  id: string;
  title: string;
  subtitle: string;
  path: string;
  dirname: string;
  /** Null for a detached worktree. Never inferred from the directory name. */
  branch: string | null;
  head: string | null;
  isMain: boolean;
  isBare: boolean;
  locked: string | null;
  prunable: string | null;

  dirty: boolean;
  untracked: number;
  staged: number;
  ahead: number;
  behind: number;

  issueKey: string | null;

  badges: Badge[];
  links: Link[];
  table: TableRow[];
  /**
   * Environment key *names* only, sorted. No value is ever sent with the listing — fetch
   * one at a time with `commands.revealEnvValue`.
   */
  env: string[];
  /** The rendered `[browser] home`, or the first visible, openable HTTP(S) display link. */
  browserHome: string | null;
}

/**
 * One group in the sidebar. Mirrors `SidebarGroupView` in `view.rs`.
 *
 * `favorites` and `ungrouped` are the two built-in ids; see `sidebar.ts`.
 */
export interface SidebarGroup {
  id: string;
  /** Empty for the built-ins, whose labels are the frontend's copy. */
  name: string;
  collapsed: boolean;
  /** Worktree ids, in display order. May name worktrees git no longer lists. */
  worktrees: string[];
}

/**
 * How the sidebar arranges a project's worktrees. Mirrors `SidebarView` in `view.rs`.
 *
 * The one type that also travels back: the frontend edits it locally and sends the whole thing
 * to `set_sidebar_layout`, which answers with it normalized.
 */
export interface SidebarLayout {
  groups: SidebarGroup[];
  /** Starred worktree id → the custom group it was starred from, so unstarring can go back. */
  origins: Record<string, string>;
}

export type DatabaseEngine = 'postgres' | 'mysql' | 'sqlite';
export type DatabaseScope = 'worktree' | 'project';
export type DatabaseEnvironment = 'local' | 'test' | 'staging' | 'production';
export type DatabaseAccess = 'read_write' | 'read_only';
export type DatabaseTls = 'disable' | 'require';

export interface DatabaseConnection {
  id: string;
  label: string;
  engine: DatabaseEngine;
  scope: DatabaseScope;
  environment: DatabaseEnvironment;
  access: DatabaseAccess;
  tls: DatabaseTls;
  target: string;
  available: boolean;
  problem: string | null;
}

export interface DatabaseSession {
  id: string;
  profileId: string;
  label: string;
  engine: DatabaseEngine;
  environment: DatabaseEnvironment;
  access: DatabaseAccess;
  serverVersion: string | null;
}

export interface DatabaseSchema {
  name: string;
}

export type RelationKind = 'table' | 'view' | 'materialized_view';

export interface DatabaseRelation {
  schema: string;
  name: string;
  kind: RelationKind;
}

export interface DatabaseColumn {
  name: string;
  typeName: string;
  nullable: boolean;
  default: string | null;
  primaryKey: boolean;
}

export interface QueryColumn {
  name: string;
  typeName: string | null;
}

export interface QueryCell {
  value: string | null;
  truncated: boolean;
}

export interface QueryResult {
  columns: QueryColumn[];
  rows: QueryCell[][];
  affectedRows: number;
  durationMs: number;
  truncated: boolean;
  message: string | null;
}

export interface TablePageRequest {
  schema: string;
  table: string;
  offset: number;
  limit: number;
  /** A WHERE condition as typed. The page runs read-only, so it narrows rows and changes none. */
  filter: string | null;
  /** An ORDER BY list as typed; the grid's sort toggle writes one too. */
  orderBy: string | null;
}

/** A worktree's files for the Code tab's tree. Mirrors `CodeTreeView` in `src-tauri/src/code.rs`. */
export interface CodeTree {
  /** Tracked files and untracked ones that are not ignored, relative to the worktree. */
  files: string[];
  /** Ignored paths at their shallowest ignored level. A directory ends in `/`. */
  ignored: string[];
  /** Listed paths that are directories on disk: submodules, and links to directories. */
  dirs: string[];
  symlinks: string[];
  /** Listed, but deleted from the working tree. */
  missing: string[];
  /** A listing was cut short, so the tree is not the whole worktree. */
  truncated: boolean;
}

/** One entry of a directory git did not list. Mirrors `CodeEntryView`. */
export interface CodeEntry {
  name: string;
  kind: 'file' | 'dir' | 'other';
  symlink: boolean;
}

/** A file for the viewer. Mirrors `CodeFileView`. */
export interface CodeFile {
  /** Null for a binary file, which is described rather than shown. */
  text: string | null;
  size: number;
  /** The text stops before the file does — it is past the five-mebibyte cap. */
  truncated: boolean;
  mtimeMs: number | null;
  symlink: boolean;
  /** It resolves to somewhere outside the worktree, through a link. */
  outside: boolean;
}

/** Whether an open file changed, without reading it. Mirrors `CodeStatView`. */
export interface CodeStat {
  path: string;
  exists: boolean;
  mtimeMs: number | null;
  size: number;
}

/** Find in Files' toggles. Mirrors `CodeSearchOptionsView`. */
export interface CodeSearchOptions {
  caseSensitive: boolean;
  wholeWord: boolean;
  regex: boolean;
  /** Comma-separated globs: `*.py, !*.min.js`. A glob with a `/` is a path from the root. */
  mask: string;
  includeIgnored: boolean;
}

/** One matching line. Mirrors `CodeHitView`. */
export interface CodeHit {
  path: string;
  /** 1-based. */
  line: number;
  /** The line, or a window of a long one around its first match. */
  text: string;
  /** Where `text` starts in the line, in UTF-16 units. */
  offset: number;
  /** `[start, end)` in UTF-16 units into `text` — a JS string's own indexing. */
  ranges: [number, number][];
}

/** Mirrors `CodeSearchView`. */
export interface CodeSearch {
  hits: CodeHit[];
  matches: number;
  files: number;
  truncated: boolean;
  ignoredStopped: boolean;
}

/** How a file differs from the Changes view's revision. */
export type CodeChangeKind =
  'added' | 'modified' | 'deleted' | 'renamed' | 'untracked' | 'other';

/** Mirrors `CodeChangeView`. */
export interface CodeChange {
  path: string;
  kind: CodeChangeKind;
  /** Where a renamed file was. */
  from: string | null;
}

/** Mirrors `CodeChangesView`. */
export interface CodeChanges {
  /** The scope actually used: a branch with no base falls back to uncommitted. */
  scope: 'branch' | 'uncommitted';
  /** The base branch's name, or `HEAD`. */
  against: string;
  /** Handed back to `codeFileDiff` and `codeBaseVersion` as given. */
  rev: string;
  changes: CodeChange[];
}

/** Mirrors `CodeHunkView`. A zero count means that side has no lines here. */
export interface CodeHunk {
  oldStart: number;
  oldLines: number;
  newStart: number;
  newLines: number;
  removed: string[];
}

/** A comment on lines in the Code tab. Mirrors `CodeComment` in `code_comments.rs`. */
export interface CodeComment {
  id: number;
  path: string;
  /** 1-based and inclusive. */
  start: number;
  end: number;
  /** The lines as they were when the comment was written. */
  excerpt: string;
  text: string;
  status: 'open' | 'resolved';
  /** The agent it was last drafted to, by the label the user saw. */
  sentTo: string | null;
  /** What an agent said when it resolved it. */
  note: string | null;
}

/** Mirrors `NewComment`. */
export interface NewCodeComment {
  path: string;
  start: number;
  end: number;
  excerpt: string;
  text: string;
}

export type CodeSymbolKind =
  | 'class'
  | 'function'
  | 'method'
  | 'interface'
  | 'type'
  | 'enum'
  | 'struct'
  | 'trait'
  | 'module'
  | 'constant';

/** One definition. Mirrors `CodeSymbolView`. */
export interface CodeSymbol {
  name: string;
  kind: CodeSymbolKind;
  container: string | null;
  path: string;
  line: number;
}

export interface Field {
  key: string;
  label: string;
  kind: FieldKind;
  required: boolean;
  default: string | null;
  placeholder: string | null;
  help: string | null;
  allowCustom: boolean;
  /** True when options come from a command, so the UI must fetch them. */
  hasDynamicOptions: boolean;
  options: string[];
  pattern: string | null;
  patternMessage: string | null;
}

export interface Action {
  id: string;
  label: string;
  pty: boolean;
}

/** One external tool the selected worktree can be opened in. */
export interface Opener {
  id: string;
  label: string;
  available: boolean;
  /**
   * Why it cannot be used, for a tooltip. Null when it can.
   *
   * Composed in Rust because the useful version of this sentence names the program that
   * was searched for, which only the catalogue knows.
   */
  detail: string | null;
}

export interface Openers {
  openers: Opener[];
  /** Which one the primary half of the split button runs. Null only if none exist. */
  preferred: string | null;
}

export interface Form {
  projectId: string;
  fields: Field[];
  actions: Action[];
}

export interface Preflight {
  id: string;
  severity: PreflightSeverity;
  message: string;
  /** True when the user may proceed anyway by acknowledging it. */
  overridable: boolean;
  hint: string | null;
}

export interface BranchChoice {
  branch: string;
  remoteOnly: boolean;
  directory: string;
}

/** The review screen: exactly what will happen, before anything has. */
export interface Preview {
  branch: string | null;
  directory: string;
  baseRef: string;
  baseCommit: string | null;
  willFetch: boolean;
  /** The literal `git worktree add …` argv that will run. */
  gitArgv: string[];
  setupArgv: string[] | null;
  /** Where setup runs — often the repo root, not the new worktree. */
  setupCwd: string | null;
  preflight: Preflight[];
  warnings: string[];
  lookups: Record<string, string>;
  computed: Record<string, string>;
  branchChoices: BranchChoice[];
  /**
   * Field keys that feed the branch and directory templates.
   *
   * Adopting an existing branch supplies both, so these inputs go inert — while every other
   * field still drives the setup command. Derived in Rust from the project's own templates.
   */
  namingFields: string[];
  /** Field values after normalization, so the form can show `1234` → `ACME-1234`. */
  normalized: Record<string, string>;
  canCreate: boolean;
}

export type CreateOutcome =
  | { kind: 'created'; worktree: Worktree; setupSession: string | null }
  | {
      kind: 'setup_failed';
      worktree: Worktree;
      session: string;
      outcome: ExitOutcome;
      remedies: Remedy[];
    }
  | { kind: 'cancelled'; worktree: Worktree | null; session: string | null };

export type Remedy =
  { kind: 'retry_setup' } | { kind: 'open_shell' } | { kind: 'remove_worktree' };

export type ExitOutcome =
  | { kind: 'success' }
  | { kind: 'failed'; code: number }
  | { kind: 'signalled'; signal: number }
  | { kind: 'timed_out'; afterMs: number }
  | { kind: 'cancelled' };

// ── reviews: pull requests in the Work view's queue ──

/** The forge a project's `[workflow] forge` names. One, so far. */
export type ForgeKind = 'github';

/** A pull request in the Reviews queue, as the flow store keeps it. */
export interface ReviewItem {
  id: number;
  /** `owner/name`. */
  repo: string;
  number: number;
  title: string;
  /** The worktree last used to review it, by path. It may have been removed since. */
  worktree: string | null;
  /** When you said "I'm reviewing this". Kept in wtm, never sent to GitHub. */
  reviewingSince: number | null;
}

export interface ReviewQueue {
  /** Null when the project's workflow names no forge: nothing can be added, and the view says why. */
  forge: ForgeKind | null;
  items: ReviewItem[];
}

export interface RepoSlug {
  host: string;
  owner: string;
  name: string;
}

export type PrState = 'open' | 'closed' | 'merged' | 'unknown';
export type ChecksState = 'none' | 'failing' | 'pending' | 'passing';
export type Permission =
  'unknown' | 'none' | 'read' | 'triage' | 'write' | 'maintain' | 'admin';

export interface PullRequest {
  number: number;
  url: string;
  title: string;
  author: string | null;
  state: PrState;
  draft: boolean;
  /** `repo` is null when the fork the pull request came from has been deleted. */
  head: { repo: RepoSlug | null; branch: string; sha: string };
  base: { repo: RepoSlug; branch: string; sha: string | null };
  /** From a fork. */
  crossRepository: boolean;
  maintainerCanModify: boolean;
  requestedReviewers: { name: string; team: boolean }[];
  checks: { passed: number; failed: number; pending: number; skipped: number };
}

export interface BranchProtection {
  branch: string;
  /** Null when the forge would not say. */
  protected: boolean | null;
  rules: string[];
}

export interface ForgeAccount {
  host: string;
  login: string | null;
  version: string | null;
}

/** One pull request, as the forge describes it now. */
export interface ReviewDetail {
  item: ReviewItem;
  pullRequest: PullRequest;
  checksState: ChecksState;
  permission: Permission;
  canPush: boolean;
  /** The base branch is one of the workflow's integration branches. */
  baseIntegration: boolean;
  /** Read only when the base is an integration branch. */
  baseProtection: BranchProtection | null;
  account: ForgeAccount;
}

export type ReviewCheckout = 'detached' | 'branch';

export interface ReviewWorktreeArgs {
  itemId: number;
  /** The head the view showed. Preparing refuses a different one. */
  expectedHead: string | null;
  checkout: ReviewCheckout;
  /** Run the project's setup in the new worktree. Off unless asked for. */
  prepareEnvironment: boolean;
  acknowledged?: string[];
  rows?: number;
  cols?: number;
}

export type ReuseReason =
  'review_branch' | 'review_directory' | 'pull_request_branch' | 'head_commit';

export type ReviewTarget =
  | {
      kind: 'reuse';
      worktreeId: string;
      path: string;
      reason: ReuseReason;
      atHead: boolean;
    }
  | { kind: 'create'; directory: string; branch: string | null; gitArgv: string[] };

export type ReviewSetup =
  | { kind: 'skipped' }
  | { kind: 'not_declared' }
  | { kind: 'not_applicable' }
  | { kind: 'run'; argv: string[]; cwd: string };

/** What preparing a review worktree would do, before anything has. */
export interface ReviewPreview {
  number: number;
  head: string;
  fetch: { remote: string; refspec: string } | null;
  headPresent: boolean;
  target: ReviewTarget;
  setup: ReviewSetup;
  preflight: Preflight[];
  warnings: string[];
  canCreate: boolean;
}

export type ReviewOutcome =
  | { kind: 'reused'; worktreeId: string; path: string; atHead: boolean }
  | { kind: 'created'; worktreeId: string; path: string; setupSession: string | null }
  | { kind: 'setup_failed'; worktreeId: string; path: string; session: string }
  | { kind: 'cancelled'; worktreeId: string | null };

export type RemoveOutcome =
  | { kind: 'removed'; branchDeleted: boolean; warnings: PlanWarning[] }
  | { kind: 'teardown_failed'; session: string | null; warnings: PlanWarning[] };

export interface PlanWarning {
  id: string;
  message: string;
}

export interface SetupResult {
  session: string;
  success: boolean;
  summary: string;
}

/**
 * A live shell in the terminal dock.
 *
 * Named `TerminalSession`, not `Terminal`, unlike every other view type here:
 * `Terminal.svelte` imports `Terminal` from `@xterm/xterm`, and a contract type that shadows
 * the terminal emulator is a fifteen-minute mystery waiting to happen.
 *
 * `worktree` is the worktree id — an absolute path — so panes key off the same string the
 * sidebar uses. This is the only reliable answer to "does this worktree already have a
 * terminal": a reload wipes this side's map while the shells keep running, and Rust is the
 * only place that still knows.
 */
export interface TerminalSession {
  session: string;
  worktree: string;
  project: string;
}

/** Emitted as `pty:output`. Bytes are base64 because JSON cannot carry them. */
export interface PtyOutput {
  session: string;
  chunkBase64: string;
  /** The chunk's number in a dock shell's replay; `null` for every other session. */
  seq: number | null;
}

/** What `terminal_replay` returns: everything kept, and the number of its last chunk. */
export interface TerminalReplay {
  through: number;
  chunkBase64: string;
}

/**
 * A pane in a window of its own, and the frontend's last record of it.
 *
 * `state` is opaque to Rust by design — it is `sessions.svelte.ts`'s own snapshot of the pane,
 * which only that file reads. See `pane_windows.rs`.
 */
export interface PaneWindow {
  paneId: string;
  state: unknown;
}

/** Emitted as `pty:exit`. */
export interface PtyExit {
  session: string;
  outcome: ExitOutcome;
  summary: string;
}

/**
 * One agent wtm can start, and whether this machine can.
 *
 * Unavailable entries are returned too, with the reason — the same contract `Opener` keeps, and
 * for the same reason: a greyed row naming the program it looked for is a diagnosis, where an
 * omitted row is a mystery.
 */
export interface AgentOption {
  id: string;
  label: string;
  blurb: string;
  available: boolean;
  /**
   * Whether the *repository* offers it — a different refusal from not being installed.
   *
   * Separate from `available` because the two have different fixes, and only one of them is about
   * the user's machine. True when no project is in scope.
   */
  offered: boolean;
  detail: string | null;
  /**
   * The repository's `[agent.<id>]` settings, which a new pane starts from ahead of the provider's
   * own defaults. Null for each one the repository leaves unset, and for all three with no project
   * in scope.
   */
  model: string | null;
  effort: string | null;
  mode: string | null;
}

/**
 * A live agent session.
 *
 * Keyed by `session`, unlike {@link TerminalSession} which is keyed by worktree: a worktree may
 * have several agent sessions at once, which is the point of the feature.
 */
export interface AgentSession {
  session: string;
  worktree: string;
  project: string;
  provider: string;
  /**
   * The provider's own id for this conversation, or `""` before its handshake names one.
   *
   * What a re-attaching window matches a live session against, so it lands in the pane it was in.
   */
  providerSession: string;
  /** Set for a workflow run's session, which a re-attaching window keeps off the tiles. */
  flow: FlowRole | null;
}

/**
 * Which run, and which role in it, a workflow run's session holds. Mirrors Rust's `FlowRole`.
 *
 * The authority is a union, not a string, so a typo in a comparison is a type error here rather
 * than a writer treated as a reviewer.
 */
export interface FlowRole {
  flow: number;
  role: string;
  authority: 'reviewer' | 'writer';
}

export interface EffortOption {
  effort: string;
  description: string | null;
}

/**
 * One model a provider offers, and the effort ladder **that model** supports.
 *
 * Per model, not per provider, because that is what the providers report: `gpt-6-astra` offers six
 * efforts including `ultra` and `gpt-5.5` offers four. A picker built on a per-provider ladder would
 * offer rungs the selected model rejects.
 */
export interface AgentModel {
  id: string;
  label: string;
  description: string | null;
  isDefault: boolean;
  /**
   * The permission mode this model's semantics assume, or null. A seed the pickers apply on
   * selection, never a lock — `opusplan` is Opus only while the session is in plan mode.
   */
  impliedMode: string | null;
  defaultEffort: string | null;
  efforts: EffortOption[];
}

/**
 * How much a permission mode lets a session do without being asked.
 *
 * Three tiers rather than a boolean because the middle one is real: `acceptEdits` writes files
 * without asking but still gates commands. The composer's mode control takes its colour from this,
 * and it is decided in Rust rather than by a substring test here — a `name.includes('bypass')`
 * check would rate Codex's `danger-full-access` as safe.
 */
export type ModeRisk = 'normal' | 'elevated' | 'unsandboxed';

/** One permission or approval mode a provider offers. */
export interface AgentMode {
  /** The provider's own spelling. What goes on the wire, unchanged. */
  id: string;
  label: string;
  description: string | null;
  isDefault: boolean;
  risk: ModeRisk;
}

/**
 * One thing a session can be asked to do by name — the composer's `/` list.
 *
 * A Claude slash command and a Codex skill are the same affordance under two names. `description`
 * is always null for Claude, whose init line reports names and nothing else, so a missing one is
 * ordinary rather than an error.
 */
export interface AgentSkill {
  name: string;
  description: string | null;
  /** Codex says `user`, `repo`, `system` or `admin`. Null where the provider does not say. */
  scope: string | null;
}

/**
 * Whether dictation can be offered on this machine.
 *
 * `keySet` is a boolean and never the key. ARCHITECTURE §6a's rule is that no *value* crosses this
 * boundary rather than no secret, and a transcription key is a value — so it goes in through
 * `setDictationKey` and nothing brings it back.
 */
export interface DictationStatus {
  /** Everything needed is installed and a key is stored. */
  ready: boolean;
  keySet: boolean;
  /** Programs that are needed and missing, so a message can name them. */
  missing: string[];
}

/**
 * What the update check found. `available` is computed in Rust, so the frontend never compares
 * versions itself.
 */
export interface UpdateStatus {
  current: string;
  latest: string;
  available: boolean;
  /** The release's page, built from the version rather than taken from GitHub's reply. */
  url: string;
  /** `homebrew` when Homebrew installed this copy and can upgrade it; `download` otherwise. */
  via: 'homebrew' | 'download';
}

/** How the update the previous run started went. Reported once, on the next launch. */
export type UpdateOutcome =
  | { kind: 'updated'; version: string }
  /** `log` is the end of `~/.config/wtm/update.log`, where Homebrew says what went wrong. */
  | { kind: 'failed'; version: string; log: string };

/** What an agent can do on this machine. */
export interface Capability {
  models: AgentModel[];
  modes: AgentMode[];
  /**
   * True when the models came from asking the CLI rather than from a table compiled into wtm.
   *
   * Codex answers `model/list`; Claude Code has no such call, so its list is as of this build. The UI
   * says which, because a stale list being the CLI's fault and being ours are different problems.
   */
  modelsAreLive: boolean;
  /**
   * True where this provider has a high-speed mode, so the picker can offer the control.
   *
   * Claude has one; Codex and Cursor do not, because their speed axis *is* the effort ladder on
   * every model. Gating on this rather than on a provider-id test, so the pill appears where the
   * word means something and the check does not have to be repeated per provider.
   */
  supportsFast: boolean;
  /**
   * True where a message steered into a running turn reaches it without stopping it.
   *
   * Claude and Codex take one at the next step. Cursor's protocol runs one prompt at a time, so its
   * steer cancels the prompt and sends a new one — the queue's "Send now" says so before it is
   * pressed, which is what this is for.
   */
  steersMidTurn: boolean;
}

/**
 * A conversation that can be picked up again.
 *
 * What persists across a quit is this handle, not a session: the child process is gone, but both CLIs
 * keep the transcript and will hand it back given the id they know it by. So wtm offers what *can* be
 * resumed and re-establishes on demand, rather than respawning a fleet of CLIs on launch.
 */
export interface Resumable {
  provider: string;
  providerSession: string;
  title: string | null;
  model: string | null;
  effort: string | null;
  updated: string | null;
}

/**
 * A stored plan.
 *
 * Called a Brief because `wtm-core` has owned the word `Plan` since v0.1 for the create pipeline's
 * preview, and because it is what these are: a document written to be handed to someone else.
 */
export interface Brief {
  id: string;
  title: string;
  provider: string;
  created: string;
  markdown: string;
}

/** One background agent, as its CLI reports it. */
export interface BackgroundTask {
  id: string;
  name: string;
  /** The CLI's own word — `done`, `failed`, `blocked`, `running`. Not normalized. */
  state: string;
  session: string | null;
}

export interface AgentUsage {
  tokensIn: number;
  tokensOut: number;
  cached: number;
  /** Best available numerator for the session's context-window meter. */
  contextUsed: number;
  contextWindow: number | null;
}

/**
 * One rolling allowance on a provider account. Mirrors `wtm_core::model::LimitWindow`.
 *
 * Identified by its length rather than a name, because the providers' names (`primary`,
 * `five_hour`) do not say what the window is. `limitLabel` in `usage.svelte.ts` names it.
 */
export interface LimitWindow {
  /** 300 for five hours, 10080 for a week. `null` when the provider did not say. */
  minutes: number | null;
  /** What the window is narrowed to, such as one model, or `null` for the whole account. */
  scope: string | null;
  /** 0 to 100, whatever the provider sent: the conversion happens in Rust. */
  usedPercent: number;
  /** Unix *seconds*, as the provider stated it. */
  resetsAt: number | null;
}

/** What one provider account has left. Mirrors `wtm_core::model::UsageLimits`. */
export interface UsageLimits {
  provider: string;
  plan: string | null;
  /** Shortest first, account-wide windows ahead of narrower ones. */
  windows: LimitWindow[];
  /** Free full resets granted and not yet used. Codex only. */
  resetCredits: number | null;
}

/**
 * One provider's record in the app's usage registry, from `usage_limits`,
 * `refresh_usage_limits` and the `usage:limits` event. See `src-tauri/src/usage.rs`.
 */
export interface AccountUsage {
  provider: string;
  limits: UsageLimits;
  /**
   * Unix *milliseconds* at which the windows were last reported.
   *
   * Not the same as `askedAt`: asking Claude gets its plan, and only a turn gets its windows.
   */
  reportedAt: number | null;
  /** Unix milliseconds at which the provider was last asked directly. */
  askedAt: number | null;
  /** Why the last direct ask failed, in the CLI's words. The last good figures are kept. */
  error: string | null;
  /** How the provider is signed in, asked the way a workflow run would. `null` until asked. */
  auth: AuthCheck | null;
}

/**
 * How a provider's CLI is signed in. Mirrors `wtm_core::model::AuthMethod`.
 *
 * Nothing of a key ever crosses: `api_key` names where the key came from, never the key.
 */
export type AuthMethod =
  | { kind: 'subscription'; service: string }
  | { kind: 'api_key'; source: string | null }
  | { kind: 'other'; method: string }
  | { kind: 'signed_out' }
  | { kind: 'unreadable' };

/** The subscription preflight's answer for one provider. Mirrors `wtm_core::model::AuthCheck`. */
export interface AuthCheck {
  provider: string;
  method: AuthMethod;
  /** "Signed in with claude.ai": the method, worded in Rust so the view and a refusal agree. */
  summary: string;
  /** Why a workflow run on this provider would refuse to start, or `null` when it may. */
  refusal: string | null;
  /** Names of billing variables in wtm's own environment. Panes inherit them; runs do not. */
  inherited: string[];
}

export interface AgentAttachment {
  name: string;
  /** Absolute local path. Pasted files are staged under the OS temporary directory. */
  path: string;
  mime: string;
  size: number;
  /** Used for image previews and Claude's inline image input. */
  dataBase64: string;
}

export type AgendaStatus = 'pending' | 'in_progress' | 'completed';

export interface AgendaStep {
  text: string;
  status: AgendaStatus;
}

export interface UserInputOption {
  label: string;
  description: string | null;
}

export interface UserInputQuestion {
  id: string;
  header: string;
  question: string;
  options: UserInputOption[];
  multiple: boolean;
  allowsOther: boolean;
  secret: boolean;
}

/** Something a session needs a human to decide before it can continue. */
export type ApprovalRequest =
  | { kind: 'command'; command: string; cwd: string | null; reason: string | null }
  | { kind: 'file_change'; unified_diff: string; reason: string | null }
  | { kind: 'permissions'; summary: string; items: string[] }
  | { kind: 'plan_review'; markdown: string; path: string | null }
  | { kind: 'tool_input'; tool: string; prompt: string }
  | { kind: 'user_input'; questions: UserInputQuestion[] };

/**
 * One thing that happened in an agent session.
 *
 * Mirrors `AgentEvent` in `crates/wtm-core/src/model/agent.rs`, which is `#[serde(tag = "kind")]`
 * with camelCase payload fields. `view::tests::an_agent_event_is_tagged_by_kind_with_camel_case_payloads`
 * pins the tag and the casing so this union cannot silently drift.
 *
 * **`raw` is not a fallback, it is the design.** Both CLIs' protocols are experimental and will
 * grow event kinds inside a patch release, so an unrecognised one arrives here rather than
 * breaking the transcript. Render it as a collapsed row; never drop it.
 */
export type AgentEvent =
  | {
      kind: 'session_ready';
      providerSessionId: string;
      model: string | null;
      effort: string | null;
      /**
       * The mode the provider resolved to, which is not always the one wtm asked for: a resumed
       * Claude conversation keeps its own.
       */
      mode: string | null;
      tools: string[];
    }
  | { kind: 'skills_listed'; skills: AgentSkill[] }
  /** The provider dropped the mode it was started in, as Claude does with an Auto it cannot keep. */
  | { kind: 'mode_changed'; mode: string }
  | { kind: 'turn_started'; turn: string }
  | { kind: 'turn_finished'; turn: string; usage: AgentUsage; costUsd: number | null }
  | { kind: 'attachments'; attachments: AgentAttachment[] }
  | { kind: 'user_echo'; text: string }
  | { kind: 'message_delta'; text: string }
  | { kind: 'message'; text: string }
  | { kind: 'reasoning_delta'; text: string }
  | { kind: 'tool_started'; id: string; name: string; title: string | null }
  | { kind: 'tool_finished'; id: string; ok: boolean; output: string | null }
  | { kind: 'command_started'; id: string; command: string; cwd: string | null }
  | { kind: 'command_output'; id: string; chunk: string }
  | { kind: 'command_finished'; id: string; exitCode: number | null }
  | { kind: 'patch'; id: string; unifiedDiff: string }
  | { kind: 'agenda_updated'; explanation: string | null; steps: AgendaStep[] }
  | { kind: 'approval_requested'; id: string; blocking: boolean; request: ApprovalRequest }
  | { kind: 'approval_resolved'; id: string }
  | {
      kind: 'usage';
      tokensIn: number;
      tokensOut: number;
      cached: number;
      contextUsed: number;
      contextWindow: number | null;
    }
  | { kind: 'notice'; level: 'info' | 'warn'; message: string }
  | { kind: 'failed'; message: string }
  /**
   * Out of tokens. Separate from `failed` because the remedy differs — see the Rust variant's
   * docs and `SessionPane`'s limit banner, which offers to continue on the other provider.
   *
   * `resetsAt` is Unix *seconds*, not milliseconds: it crosses the wire as the provider stated it.
   * Multiply before handing it to `Date`.
   */
  | { kind: 'limit_reached'; message: string; resetsAt: number | null }
  | { kind: 'raw'; provider: string; event: string; payload: unknown };

/**
 * What the user answered.
 *
 * `allow_with_edits` is Claude Code only — its allow can carry a replacement payload and rewrite
 * the call. Codex refuses the answer rather than running the original unedited, so the UI must not
 * offer the affordance where it cannot be honoured. Nothing in the UI sends it today: the button
 * that did had no editor behind it and sent an empty payload, which read as a plain approve with
 * the tool's arguments erased. See `ApprovalCard`.
 */
export type ApprovalAnswer =
  | { kind: 'allow' }
  | { kind: 'allow_for_session' }
  | { kind: 'allow_with_edits'; input: unknown }
  | { kind: 'deny'; message: string | null }
  | { kind: 'user_input'; answers: Record<string, string[]>; notes: string | null };

/**
 * Emitted as `notification:clicked` — the user clicked a macOS notification.
 *
 * Mirrors `ClickPayload` in `wtm-notify`, whose round-trip test pins this exact shape.
 * `paneId` is process-local and best-effort: the notification can outlive the pane it was
 * about, so `App.svelte` navigates on (project, worktree) and focuses the pane only if it
 * still exists.
 */
export interface NotificationClick {
  projectId: string;
  worktreeId: string;
  paneId: string;
  /**
   * The inbox item a workflow run's notification is about, which outlives the process where a
   * pane does not. Absent for a pane's own notification.
   */
  inboxItem?: number | null;
}

/** Emitted as `agent:event`. */
export interface AgentEventEnvelope {
  session: string;
  /**
   * This event's position in the session's stream.
   *
   * `null` when the backend had no registry entry to number it against — a session already gone.
   * A pane that repainted from the replay buffer uses it to skip what it has already drawn.
   */
  seq: number | null;
  event: AgentEvent;
}

/** One buffered event, as `agent_replay` returns it. */
export interface SeqEvent {
  seq: number;
  event: AgentEvent;
}

/** Emitted as `agent:exit`. */
export interface AgentExit {
  session: string;
  outcome: ExitOutcome;
  summary: string;
}

/** Emitted as `agent:ready` — the handshake finished and turns may be sent. */
export interface AgentReady {
  session: string;
}

/**
 * Emitted as `agent:spawned` — Rust opened a session nothing in the UI asked for.
 *
 * The inverse of every other session in the app. Normally the frontend calls `openAgentSession` and
 * is handed an id; a handoff is started by a child process, so the session is already running by the
 * time this window could know about it. Without adopting it, a CLI would be streaming into a pane
 * that does not exist.
 *
 * Carries the model, effort and mode because an adopted pane never chose them, and a picker with
 * nothing in it would suggest the session had no model rather than one this window did not pick.
 */
export interface SpawnedSession {
  session: string;
  project: string;
  worktree: string;
  provider: string;
  model: string | null;
  effort: string | null;
  mode: string | null;
  parentSession: string | null;
  run: string | null;
  title: string | null;
  /**
   * The Home session that opened this one into its worktree, where it is tiled like a hand-opened
   * pane. `parentSession` is then null: it is not a delegation child behind a rail.
   */
  openedBy: string | null;
  /**
   * A workflow run's session. `parentSession` is null for these too, and they are never tiled:
   * they sit behind their run's rail, grouped by `run`.
   */
  flow: FlowRole | null;
}

/**
 * One prompt one agent session sent another, and what came of it. Mirrors `messages::Exchange`.
 *
 * Announced on `agent:message` when it is sent and again when it settles, as the whole record both
 * times, so the window replaces by `id` rather than applying deltas. `from` and `to` are backend
 * session ids; `from` is null only for a sender that had not finished starting.
 */
export interface AgentExchange {
  id: number;
  run: string | null;
  from: string | null;
  to: string;
  via: 'ask_agent' | 'spawn_agents' | 'open_session' | 'message_session';
  /** The start of the prompt. */
  prompt: string;
  /** The start of the reply, once there is one. */
  reply: string | null;
  error: string | null;
  state: 'in_flight' | 'answered' | 'failed';
  /** Unix milliseconds. */
  sentAt: number;
  settledAt: number | null;
}

/**
 * A worktree the Home agent is creating or removing, or did lately. Mirrors `home::Job`.
 *
 * Announced whole on `home:worktree` at every step, and never on `wtm:progress`, which a New
 * Worktree form open at the same time would take for its own.
 */
export interface HomeJob {
  id: number;
  kind: 'create' | 'remove';
  projectId: string;
  projectName: string;
  /** The Home session that asked for it. */
  by: string;
  branch: string | null;
  directory: string;
  phase: 'running' | 'created' | 'setup_failed' | 'removed' | 'failed';
  step: { label: string; index: number; total: number } | null;
  /** The job's terminal: a creation's setup once it starts, or the teardown step that stopped a removal. */
  setupSession: string | null;
  /** The worktree's id: a creation's once git has made it, a removal's from the start. */
  worktree: string | null;
  error: string | null;
}

/**
 * One browser pane, as Rust sees it.
 *
 * The reply to `openBrowser`, a row of `listBrowsers`, and the payload of both `browser:state` and
 * `browser:opened` — one shape, so the frontend mirrors it whole and never merges a delta.
 */
export interface BrowserView {
  prototypeId?: string | null;
  /** The webview label, `browser-…`. Never a session id. */
  id: string;
  project: string;
  worktree: string;
  url: string;
  title: string;
  loading: boolean;
  /**
   * Why the last navigation showed no page — WebKit's own sentence — until the next one starts.
   * While set, `url` is the address that failed.
   */
  loadError: string | null;
  canGoBack: boolean;
  canGoForward: boolean;
  /** Whether agents may drive this pane. The per-pane toggle, on by default. */
  agentAccess: boolean;
  /** The agent session that opened it, when one did. */
  openedBy: string | null;
  commentMode: boolean;
  /** The label of the agent acting on the page, while one is. */
  agentDriving: string | null;
}

/** Where a browser's tile is, in this webview's CSS pixels. Logical units end to end. */
export interface BrowserBounds {
  x: number;
  y: number;
  w: number;
  h: number;
}

export type BrowserHistoryAction = 'back' | 'forward' | 'reload' | 'stop';

/** Emitted as `browser:closed`. `summary` is a sentence when the pane should say why it ended. */
export interface BrowserClosed {
  id: string;
  summary: string | null;
}

/** Whether this build can show a browser pane, drive it, and why not when it cannot. */
export interface BrowserAvailability {
  /** Whether a pane can be shown at all. */
  available: boolean;
  /** Whether the runtime — agent tools, comments, snapshots — can be installed. macOS only, so far. */
  runtime: boolean;
  reason: string | null;
}

/** Where a comment's element was, in page coordinates, when the comment was made. */
export interface BrowserAnchorRect {
  x: number;
  y: number;
  w: number;
  h: number;
}

/** What a comment is attached to, as the page runtime described the element at the click. */
export interface BrowserCommentAnchor {
  selector: string;
  tag: string;
  role: string | null;
  name: string | null;
  text: string;
  rect: BrowserAnchorRect | null;
  url: string;
  pageTitle: string;
  nearestHeading: string | null;
  styles: Record<string, string> | null;
}

/** One comment the user left on an element. `id` counts from one per browser. */
export interface BrowserComment {
  id: number;
  text: string;
  status: 'open' | 'resolved';
  anchor: BrowserCommentAnchor;
}

/** Emitted as `browser:comments` — the whole list, never a delta. */
export interface BrowserComments {
  id: string;
  comments: BrowserComment[];
}

/** Emitted as `browser:pick` — an element was picked in comment mode, or a pin was clicked. */
export interface BrowserPick {
  id: string;
  commentId: number | null;
}

/** Emitted as `browser:shortcut` — a chord or gesture happened inside the page. */
export interface BrowserShortcutEvent {
  id: string;
  action: string;
}

/** Emitted as `wtm:progress` while a pipeline runs. */
export type ProgressEvent =
  | { kind: 'stage'; id: string; label: string; index: number; total: number }
  | { kind: 'lookup_started'; id: string }
  | { kind: 'lookup_finished'; id: string; tokens: Record<string, string> }
  | { kind: 'command_started'; argv: string[]; cwd: string }
  | { kind: 'command_finished'; argv: string[]; code: number; durationMs: number }
  | { kind: 'session_started'; session: string }
  | { kind: 'warning'; id: string; message: string }
  | { kind: 'note'; message: string };

export interface Tool {
  name: string;
  path: string | null;
}

export interface Doctor {
  resolvedPath: string;
  pathSource: string;
  configDir: string;
  tools: Tool[];
}

/**
 * A palette declared in `[ui.palettes]`.
 *
 * Only ever the user's own. The nine built-ins live in the stylesheet as CSS custom
 * properties and never cross this boundary — see `PALETTES` in `state/theme.svelte.ts`
 * for the list the picker shows alongside these.
 *
 * `error` non-null means the declaration is unusable and `brand` is empty. Settings shows
 * it disabled with the reason attached rather than hiding it, so an entry that is in the
 * config file but not in the picker is never a silent mystery.
 */
export interface Palette {
  id: string;
  name: string;
  hue: number;
  chroma: number;
  /** The accent ramp at 300, 400, 500, 600. Empty when `error` is set. */
  brand: string[];
  error: string | null;
}

/** The error shape every command rejects with. */
export interface WtmError {
  kind: string;
  message: string;
  detail: unknown;
}

/** Narrow an unknown rejection to {@link WtmError}. */
export function isWtmError(value: unknown): value is WtmError {
  return (
    typeof value === 'object' &&
    value !== null &&
    'kind' in value &&
    'message' in value &&
    typeof (value as WtmError).message === 'string'
  );
}

/** A human-readable message for anything thrown across the IPC boundary. */
export function errorMessage(value: unknown): string {
  if (isWtmError(value)) return value.message;
  if (value instanceof Error) return value.message;
  if (typeof value === 'string') return value;
  return 'Something went wrong.';
}

// ─────────────────────────────── review runs ───────────────────────────────
//
// The Rust shapes in `wtm_core::flow` and `src-tauri/src/flow`, as they serialize. Unions where Rust
// has an enum, so a status the readout does not handle is a type error, not a blank row.

export type RoleProvider = 'claude' | 'codex';

export type FlowPhase =
  | 'resolving'
  | 'preparing'
  | 'investigating'
  | 'planned'
  | 'designing'
  | 'design_ready'
  | 'implementing'
  | 'verifying'
  | 'reviewing'
  | 'fixing'
  | 'ready_to_test'
  | 'revising'
  | 'publishing'
  | 'in_review'
  | 'consolidated'
  | 'fix_ready'
  | 'submitting'
  | 'following';

export type FlowHold =
  | { kind: 'awaiting_answer' }
  | { kind: 'awaiting_approval' }
  | { kind: 'waiting_capacity'; provider: string; resetsAt: number | null }
  | { kind: 'paused' }
  | { kind: 'interrupted'; cause: 'quit' | 'crash' | 'sleep' }
  | { kind: 'blocked'; cause: 'env' | 'permission' | 'conflict' | 'stale_remote' };

export type FlowEnd =
  { kind: 'done' } | { kind: 'failed'; reason: string } | { kind: 'cancelled' };

export interface FlowState {
  kind: 'implement' | 'review_mine' | 'review_pr' | 'revise';
  phase: FlowPhase;
  hold: FlowHold | null;
  end: FlowEnd | null;
}

export type StepStatus =
  | 'pending'
  | 'running'
  | 'waiting_capacity'
  | 'done'
  | 'failed'
  | 'replaced'
  | 'interrupted'
  | 'cancelled'
  | 'skipped';

export type InvocationStatus =
  | 'opening'
  | 'running'
  | 'submitted'
  | 'failed'
  | 'timed_out'
  | 'limit_reached'
  | 'interrupted'
  | 'cancelled';

export interface ReportedFinding {
  title: string;
  severity: 'minor' | 'notable' | 'blocking';
  class: 'confirmed_bug' | 'rule_violation' | 'conflict' | 'suggestion';
  body: string;
  path?: string | null;
  line?: number | null;
  endLine?: number | null;
  verification?: string | null;
  rule?: { id: number; revision: number } | null;
  between?: string[];
  requirement?: string | null;
  evidence?: string[];
}

/** A role's result, exactly as `flow_submit` accepted it. */
export type RoleResult =
  | { kind: 'findings'; payload: { summary?: string | null; findings: ReportedFinding[] } }
  | { kind: 'plan'; payload: { summary: string; steps: string[] } }
  | { kind: 'handoff'; payload: { summary: string } };

export interface ReviewStep {
  id: number;
  stage: number;
  role: string;
  provider: RoleProvider;
  model: string | null;
  /** What the role could do: read and report, or edit the worktree. */
  profile: 'reviewer' | 'writer';
  /** What it read: the snapshot checkout, or the worktree itself. */
  workspace: 'snapshot' | 'worktree';
  status: StepStatus;
  failures: number;
  retries: number;
  standsInFor: string | null;
  detail: string | null;
  resetsAt: number | null;
  /** Set when the role cannot run at all for want of a skill: the recipe has to change. */
  skillProblem: SkillProblem | null;
  result: RoleResult | null;
}

/** R2's `SkillProblem`: which skill, and which provider did not list it. */
export type SkillProblem =
  | { kind: 'missing'; role: string; provider: RoleProvider; skill: string }
  | { kind: 'unlisted'; role: string; provider: RoleProvider };

export interface ReviewInvocation {
  id: number;
  step: number;
  attempt: number;
  role: string;
  /** The snapshot it read. A rerun's attempts read a newer one than the first round's. */
  snapshot: number | null;
  status: InvocationStatus;
  detail: string | null;
  progress: string | null;
  /** The repository instruction files its brief carried. */
  instructionSources: string[];
  startedAt: number;
  endedAt: number | null;
}

/** Where one report came from: its attempt, its place in that attempt's submission, its role. */
export interface FindingSource {
  step: number;
  invocation: number;
  ordinal: number;
  role: string;
  provider: RoleProvider;
  model: string | null;
  workspace: 'snapshot' | 'worktree';
  treeSha: string;
}

/** A consolidated finding with every report it was made of. `wtm_core::flow::FindingRecord`. */
export interface FindingRecord extends Finding {
  /** False for a finding about a whole file: it has a path and no lines. */
  lines: boolean;
  evidence: { source: FindingSource; reported: ReportedFinding }[];
}

export type CheckStatus = 'passed' | 'failed' | 'timed_out' | 'skipped' | 'error';

/** One run of a configured check the app made for a role. The only thing that can show a pass. */
export interface CheckRun {
  id: number;
  check: string;
  role: string | null;
  status: CheckStatus;
  detail: string | null;
  exitCode: number | null;
  treeSha: string;
  startedAt: number;
  endedAt: number;
}

/** A check the run's roles may ask for. One with no `CheckRun` was not run. */
export interface CheckChoice {
  id: string;
  label: string;
  needs: string[];
  roles: string[];
}

/** What the roles were given to judge by. */
export interface AppliedKnowledge {
  rules: {
    rule: number;
    revision: number;
    kind: 'required' | 'guidance';
    text: string;
    roles: string[];
  }[];
  sources: string[];
  skills: { role: string; provider: RoleProvider; skill: string }[];
}

/** Whether a run's results still describe its worktree. `review_staleness`. */
export interface ReviewStaleness {
  flow: number;
  treeSha: string;
  headSha: string | null;
  /** Whether the worktree differs from the run's latest snapshot. */
  changed: boolean;
  paths: string[];
  /** False when git could not list every changed file: everything is then stale. */
  complete: boolean;
  staleFindings: number[];
  staleChecks: number[];
  /** What Rerun affected would run. */
  affected: string[];
  /**
   * For a teammate's pull request: its head by the last reading, when that is not the head the
   * review read. Everything above is then measured against it rather than the worktree.
   */
  pullRequestHead: string | null;
  /** Whether that head's commits are here. When not, every finding is stale until fetched. */
  pullRequestHeadHere: boolean;
}

export type RerunScope = 'affected' | 'all';

/** What an inbox item asks of you. `wtm_core::flow::InboxKind`. */
export type InboxKind =
  'question' | 'approval' | 'failure' | 'capacity' | 'interrupted' | 'review_ready';

/** Where the thing an item is about lives, and so where a click on it lands. */
export type InboxTarget =
  | { kind: 'flow' }
  | { kind: 'step'; step: number; role: string }
  | { kind: 'finding'; finding: number }
  | { kind: 'question'; question: number }
  | { kind: 'approval'; approval: number };

export type InboxState = 'open' | 'resolved' | 'dismissed';

/** Something a question points at. `wtm_core::flow::ContextLink`. */
export type ContextLink =
  | { kind: 'file'; path: string; line: number | null; note: string | null }
  | { kind: 'excerpt'; source: string; text: string }
  | { kind: 'design_revision'; prototype: string; revision: number }
  | { kind: 'mention'; text: string };

export interface QuestionAnswer {
  /** Index into the question's options, when one was picked. */
  choice: number | null;
  text: string;
  at: number;
}

/** A question a run asked. `wtm_core::flow::Question`. */
export interface Question {
  id: number;
  flow: number;
  step: number | null;
  /** The role that asked, when one did. */
  role: string | null;
  text: string;
  why: string;
  unblocks: string;
  /** The phase that waits for the answer; `null` when nothing does, as for a review role. */
  blocks: FlowPhase | null;
  context: ContextLink[];
  recommendation: string | null;
  options: string[];
  askedAt: number;
  answer: QuestionAnswer | null;
}

/** Something a run needs you to know or decide: `work_inbox` and the `flow:inbox` event. */
export interface InboxItem {
  id: number;
  flow: number;
  kind: InboxKind;
  target: InboxTarget;
  title: string;
  body: string;
  state: InboxState;
  createdAt: number;
  closedAt: number | null;
  /** The project and worktree the run is of: where a click on it arrives. */
  project: string | null;
  worktree: string | null;
  /** The question itself, for an item about one. */
  question: Question | null;
}

/** One review run, whole: `review_run` and friends. */
export interface ReviewRunView {
  flow: { id: number; state: FlowState; startedAt: number; updatedAt: number };
  review: {
    flow: number;
    project: string;
    worktree: string;
    worktreePath: string;
    recipe: string;
    base: string;
    /** The snapshot the run reads now: its latest, after a rerun. */
    snapshot: number | null;
  };
  snapshot: { headSha: string; treeSha: string; baseSha: string } | null;
  steps: ReviewStep[];
  invocations: ReviewInvocation[];
  inbox: InboxItem[];
  running: boolean;
  findings: FindingRecord[];
  checks: CheckRun[];
  expectedChecks: CheckChoice[];
  applied: AppliedKnowledge;
  /** The pull request a teammate's review is of; null for a review of your own work. */
  pullRequest: PullRequestReview | null;
  /** The run's local fixes, oldest first. The last is the current one. */
  fixes: LocalFix[];
  /** Who holds the reviewed worktree now: a writer, or a review reading it. */
  leases: WorktreeLease[];
  /** The writer roles your config has. */
  writers: WriterChoice[];
}

// ── local fixes: `wtm_core::flow::{fix, route}` ──

export type FixState = 'writing' | 'ready' | 'failed' | 'cancelled';

/** One local fix of a review's findings, by a writer, in its review worktree. */
export interface LocalFix {
  id: number;
  flow: number;
  step: number;
  role: string;
  findings: number[];
  scope: { paths: string[] };
  /** The head the review read, which the fix is measured from. */
  baseSha: string;
  headSha: string | null;
  /** What a route's approval pins as the diff it covers. */
  diffHash: string | null;
  state: FixState;
  detail: string | null;
  asked: { question: number; paths: string[] }[];
  allowed: string[];
  startedAt: number;
  endedAt: number | null;
}

export type LeaseKind = 'writer' | 'reviewer';

export interface WorktreeLease {
  id: number;
  worktree: string;
  flow: number;
  kind: LeaseKind;
  holder: string;
  acquiredAt: number;
  releasedAt: number | null;
  releasedWhy: 'completed' | 'cancelled' | 'failed' | 'recovered' | null;
}

/** A writer role a fix can be given to. */
export interface WriterChoice {
  id: string;
  provider: RoleProvider;
  model: string | null;
  objective: string;
}

/** `start_local_fix`. */
export interface StartFix {
  findings: number[];
  role: string;
}

/** `local_fix_diff`: the fix's diff, to read before choosing where it goes. */
export interface FixDiffView {
  fix: LocalFix;
  base: string;
  head: string;
  hash: string;
  files: { path: string; added: number | null; removed: number | null }[];
  patch: string;
  truncated: boolean;
  uncommitted: string[];
  outliers: string[];
}

export type RouteKind = 'follow_up' | 'suggestions' | 'push_to_branch';

/** What a route is asked for: the follow-up's title and body, or the suggestions' summary. */
export interface RouteRequest {
  kind: RouteKind;
  title: string;
  body: string;
}

export interface RouteOption {
  kind: RouteKind;
  /** Why it is closed; null when it is open. */
  unavailable: string | null;
  /** Exactly where it goes, when it is open. */
  destination: string | null;
  title: string;
  body: string;
}

export interface FixRef {
  base: string;
  head: string;
  diffHash: string;
}

export interface TheirBranch {
  repo: RepoSlug | null;
  branch: string;
  head: string;
}

/** `fix_routes`. */
export interface RouteOptions {
  fix: FixRef;
  their: TheirBranch;
  account: string | null;
  routes: RouteOption[];
}

/** Exactly where a push goes. */
export interface PushDestination {
  /** The remote's name, or null when it is pushed to by URL. */
  remote: string | null;
  url: string;
  repo: RepoSlug;
  reference: string;
}

export interface RoutePlan {
  kind: RouteKind;
  pullRequest: { repo: RepoSlug; number: number; url: string };
  their: TheirBranch;
  fix: FixRef;
  push: PushDestination;
  expect: { kind: 'absent' } | { kind: 'at'; sha: string };
  open: { repo: RepoSlug; base: string; head: string; title: string; body: string } | null;
}

/** `preview_fix_route`: exactly what a route sends, and the hash approving it takes. */
export interface RoutePreview {
  kind: RouteKind;
  hash: string;
  plan: RoutePlan | null;
  steps: string[];
  /** The follow-up pull request's body as it is sent, its marker included. */
  body: string | null;
  suggestions: PublishPreview | null;
  diffHash: string;
  head: string;
}

/** A push route's send, as the Reviews tab shows it. */
export interface RouteSendView {
  approval: {
    id: number;
    flow: number;
    intent: 'route';
    binding: Partial<
      Record<'remote' | 'ref' | 'diff' | 'commit' | 'head' | 'payload', string>
    >;
    grantedAt: number;
  };
  plan: RoutePlan;
  operations: Operation[];
  state: PublicationState;
  /** The follow-up pull request, once there is one. */
  url: string | null;
}

/** `approve_fix_route`: what approving started. */
export type ApprovedRoute =
  | { kind: 'route'; route: RouteSendView }
  | { kind: 'suggestions'; publication: PublicationView };

// ── a teammate's pull request, reviewed ──

/** Whether a fix could go to the pull request's own branch. `wtm_core::flow::PushToBranch`. */
export type PushToBranch =
  'allowed' | 'fork_without_maintainer_edits' | 'head_repository_gone' | 'no_write_access';

/** What became of a finding from a head the review moved on from. */
export type EarlierFate =
  'unchanged' | 'still_reported' | 'not_reported_again' | 'dismissed';

/** What the forge said about a pull request the last time wtm read it. */
export interface MirroredPr {
  item: number;
  pullRequest: PullRequest;
  permission: Permission;
  readAt: number;
}

/** What you chose to do with a review that writes nothing outside wtm. A publish is a publication. */
export interface ReviewOutcomeRecord {
  id: number;
  flow: number;
  kind: 'private';
  /** The head the review had read when you decided. */
  headSha: string;
  notes: string;
  findings: number[];
  decidedAt: number;
}

export interface EarlierHead {
  head: {
    id: number;
    flow: number;
    headSha: string;
    snapshot: number;
    findings: number[];
    checks: number[];
    nextHead: string;
    leftAt: number;
  };
  findings: (FindingRecord & { fate: EarlierFate })[];
  checks: CheckRun[];
}

export interface PullRequestReview {
  item: number;
  repo: string;
  number: number;
  mirror: MirroredPr | null;
  reviewing: { item: number; since: number } | null;
  reviewedHead: string | null;
  /** The pull request's head by the last reading, when it is not the head reviewed. */
  movedTo: string | null;
  pushToBranch: PushToBranch | null;
  earlier: EarlierHead[];
  /** Oldest first; the last is the current decision. */
  outcomes: ReviewOutcomeRecord[];
  /** The review's publishes, oldest first. The last is the current one. A fix's suggestions are one. */
  publications: PublicationView[];
  /** A local fix's push routes, oldest first. */
  routes: RouteSendView[];
}

// ── publishing a review: `wtm_core::flow::{publish, outbox, approval}` ──

/** The state a review is submitted with. `wtm_core::flow::ReviewEvent`. */
export type ReviewEvent = 'comment' | 'request_changes' | 'approve';

/** One finding picked for the review, in your words. */
export interface DraftComment {
  finding: number;
  body: string;
}

/** What the outcome sheet sends to be previewed and approved. */
export interface ReviewDraft {
  comments: DraftComment[];
  summary: string;
  event: ReviewEvent;
}

/** Why a picked finding posts in the summary rather than on a line. */
export type FoldReason =
  'whole_change' | 'whole_file' | 'not_changed' | 'no_diff' | 'not_in_change';

export interface InlineComment {
  finding: number;
  path: string;
  /** The first line of a range; `null` for one line. */
  startLine: number | null;
  line: number;
  body: string;
}

export interface FoldedComment {
  finding: number;
  path: string | null;
  line: number | null;
  body: string;
  reason: FoldReason;
}

/** Exactly what a publish posts, apart from its markers. Its hash is what an approval pins. */
export interface ReviewPayload {
  repo: RepoSlug;
  number: number;
  commitId: string;
  event: ReviewEvent;
  comments: InlineComment[];
  folded: FoldedComment[];
  summary: string;
}

/** One inline comment exactly as it posts, its marker included. */
export interface PostedComment {
  finding: number;
  path: string;
  startLine: number | null;
  line: number;
  body: string;
}

/** `preview_review_publish`: exactly what would post, and the hash an approval of it pins. */
export interface PublishPreview {
  review: ReviewPayload;
  hash: string;
  comments: PostedComment[];
  /** The pending review's own body: its marker and nothing else. */
  pendingBody: string;
  /** The review's body as it is submitted, its marker included. */
  body: string;
  /** Who it posts as. */
  account: string | null;
  /** The pull request is yours, so it can only be a comment. */
  ownPullRequest: boolean;
  /** The pull request's head by this read. */
  head: string;
  url: string;
}

export type OperationKind =
  | 'push_branch'
  | 'create_draft_pull_request'
  | 'update_pull_request'
  | 'mark_ready'
  | 'pending_review'
  | 'submit_review'
  | 'post_comment'
  | 'move_issue'
  | 'merge'
  | 'open_pull_request';

export type OperationState =
  'pending' | 'executing' | 'done' | 'failed' | 'needs_attention' | 'cancelled';

export type OperationPayload =
  | {
      kind: 'pending_review';
      /** The hash of the whole review this write posts part of. */
      review: string;
      repo: RepoSlug;
      number: number;
      commitId: string;
      comments: InlineComment[];
    }
  | {
      kind: 'submit_review';
      review: string;
      repo: RepoSlug;
      number: number;
      commitId: string;
      event: ReviewEvent;
      body: string;
    }
  | {
      kind: 'push_branch';
      route: string;
      destination: PushDestination;
      commit: string;
      expect: { kind: 'absent' } | { kind: 'at'; sha: string };
    }
  | {
      kind: 'open_pull_request';
      route: string;
      pullRequest: {
        repo: RepoSlug;
        base: string;
        head: string;
        title: string;
        body: string;
      };
    };

/** What an operation left on GitHub. */
export type OperationResult =
  | {
      kind: 'review';
      review: number;
      nodeId: string;
      url: string;
      /** Found on GitHub by its marker rather than sent this time. */
      adopted: boolean;
    }
  | { kind: 'pushed'; url: string; reference: string; commit: string; adopted: boolean }
  | { kind: 'pull_request'; number: number; url: string; adopted: boolean };

/** Why an operation stopped and waits for you. `wtm_core::flow::Attention`. */
export type Attention =
  | { kind: 'head_moved'; approved: string; current: string }
  | {
      kind: 'pending_review';
      review: number;
      nodeId: string;
      url: string;
      comments: number;
      startedByWtm: boolean;
    }
  | { kind: 'refused'; message: string };

export type Resolution = { kind: 'add_to_pending_review'; review: number; nodeId: string };

export interface Operation {
  id: number;
  key: { flow: number; kind: OperationKind; target: string; payloadHash: string };
  approval: number;
  position: number;
  payload: OperationPayload;
  state: OperationState;
  attempts: number;
  lastError: string | null;
  result: OperationResult | null;
  attention: Attention | null;
  resolution: Resolution | null;
  createdAt: number;
  updatedAt: number;
}

export type PublicationState =
  | 'posting'
  | 'checking'
  | 'published'
  | 'stale'
  | 'needs_you'
  | 'failed'
  | 'waiting'
  | 'cancelled';

/** One approval of a review, exactly what it covered, and the operations that post it. */
export interface PublicationView {
  approval: {
    id: number;
    flow: number;
    /** `route` for a local fix posted as suggestions. */
    intent: 'post' | 'route';
    /** The pins it covers: `payload` (the hash), `commit` and `head`, and a route's too. */
    binding: Partial<
      Record<'payload' | 'commit' | 'head' | 'remote' | 'ref' | 'diff', string>
    >;
    grantedAt: number;
  };
  review: ReviewPayload;
  /** Set when this is a local fix posted as suggestions. */
  suggestions?: { their: TheirBranch; fix: FixRef };
  operations: Operation[];
  state: PublicationState;
  /** The posted review on GitHub, once there is one. */
  url: string | null;
}

export interface RecipeChoice {
  id: string;
  label: string;
  roles: { id: string; provider: RoleProvider; model: string | null; skills: string[] }[];
}

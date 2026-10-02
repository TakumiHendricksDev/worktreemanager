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
 * One browser pane, as Rust sees it.
 *
 * The reply to `openBrowser`, a row of `listBrowsers`, and the payload of both `browser:state` and
 * `browser:opened` — one shape, so the frontend mirrors it whole and never merges a delta.
 */
export interface BrowserView {
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

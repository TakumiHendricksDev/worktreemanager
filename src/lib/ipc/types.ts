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
  contentHash: string;
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
  /** Starred by the user. Sorts to the top of the sidebar; see `workspace.ordered`. */
  favorite: boolean;

  badges: Badge[];
  links: Link[];
  table: TableRow[];
  /**
   * Environment key *names* only, sorted. No value is ever sent with the listing — fetch
   * one at a time with `commands.revealEnvValue`.
   */
  env: string[];
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
}

export interface EffortOption {
  effort: string;
  description: string | null;
}

/**
 * One model a provider offers, and the effort ladder **that model** supports.
 *
 * Per model, not per provider, because that is what the providers report: `gpt-5.6-sol` offers six
 * efforts including `ultra` and `gpt-5.5` offers four. A picker built on a per-provider ladder would
 * offer rungs the selected model rejects.
 */
export interface AgentModel {
  id: string;
  label: string;
  description: string | null;
  isDefault: boolean;
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
  contextWindow: number | null;
}

export type AgendaStatus = 'pending' | 'in_progress' | 'completed';

export interface AgendaStep {
  text: string;
  status: AgendaStatus;
}

/** Something a session needs a human to decide before it can continue. */
export type ApprovalRequest =
  | { kind: 'command'; command: string; cwd: string | null; reason: string | null }
  | { kind: 'file_change'; unified_diff: string; reason: string | null }
  | { kind: 'permissions'; summary: string; items: string[] }
  | { kind: 'plan_review'; markdown: string; path: string | null }
  | { kind: 'tool_input'; tool: string; prompt: string };

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
       * The mode the provider resolved to, which for Claude wtm cannot otherwise know: it passes
       * no `--permission-mode`, so `~/.claude/settings.json` is the only thing that decided it.
       */
      mode: string | null;
      tools: string[];
    }
  | { kind: 'skills_listed'; skills: AgentSkill[] }
  | { kind: 'turn_started'; turn: string }
  | { kind: 'turn_finished'; turn: string; usage: AgentUsage; costUsd: number | null }
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
      contextWindow: number | null;
    }
  | { kind: 'notice'; level: 'info' | 'warn'; message: string }
  | { kind: 'failed'; message: string }
  | { kind: 'raw'; provider: string; event: string; payload: unknown };

/**
 * What the user answered.
 *
 * `allow_with_edits` is Claude Code only — its allow can carry a replacement payload and rewrite
 * the call. Codex refuses the answer rather than running the original unedited, so the UI must not
 * offer the affordance where it cannot be honoured. See `ApprovalCard`'s `canEdit`.
 */
export type ApprovalAnswer =
  | { kind: 'allow' }
  | { kind: 'allow_for_session' }
  | { kind: 'allow_with_edits'; input: unknown }
  | { kind: 'deny'; message: string | null };

/** Emitted as `agent:event`. */
export interface AgentEventEnvelope {
  session: string;
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
 * Only ever the user's own. The six built-ins live in the stylesheet as CSS custom
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

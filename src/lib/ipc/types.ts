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

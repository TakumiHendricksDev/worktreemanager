/**
 * The only file in the frontend that calls `invoke`.
 *
 * Keeping it to one place is what makes the IPC surface greppable, mockable, and
 * type-safe in exactly one spot — a component that wants data calls `commands.*` and
 * cannot invent a command name or get its arguments wrong silently.
 */

import { invoke } from '@tauri-apps/api/core';

import type {
  Action,
  CreateOutcome,
  Doctor,
  Form,
  Openers,
  Palette,
  Preflight,
  Preview,
  Project,
  Registered,
  RemoveOutcome,
  SetupResult,
  Worktree,
} from './types';

export const commands = {
  // ── projects ──
  listProjects: () => invoke<Project[]>('list_projects'),
  /** Accepts any path inside a repository. Returns the resolved id, not just the list. */
  registerProject: (path: string) => invoke<Registered>('register_project', { path }),
  /** Takes the project's root — the store removes an exact key, not a prefix. */
  unregisterProject: (path: string) => invoke<Project[]>('unregister_project', { path }),

  // ── worktrees ──
  listWorktrees: (projectId: string) => invoke<Worktree[]>('list_worktrees', { projectId }),
  /** Persists to `~/.config/wtm/config.toml`. Returns nothing — see the Rust doc comment. */
  setWorktreeFavorite: (projectId: string, worktreeId: string, favorite: boolean) =>
    invoke<void>('set_worktree_favorite', { projectId, worktreeId, favorite }),

  // ── the form ──
  worktreeForm: (projectId: string) => invoke<Form>('worktree_form', { projectId }),
  /** Runs the field's options command. Separate so each dropdown fills in independently. */
  fieldOptions: (projectId: string, fieldKey: string) =>
    invoke<string[]>('field_options', { projectId, fieldKey }),
  listActions: (projectId: string) => invoke<Action[]>('list_actions', { projectId }),

  // ── create ──
  /** Stages 1–6b. Mutates nothing, so it is safe to call on every form change. */
  previewWorktree: (
    projectId: string,
    values: Record<string, string>,
    adoptBranch: string | null,
  ) => invoke<Preview>('preview_worktree', { projectId, values, adoptBranch }),

  createWorktree: (args: {
    projectId: string;
    values: Record<string, string>;
    adoptBranch: string | null;
    acknowledged: string[];
    rows: number;
    cols: number;
  }) => invoke<CreateOutcome>('create_worktree', args),

  // ── remove ──
  removePreflight: (projectId: string, worktreeId: string, deleteBranch: boolean) =>
    invoke<Preflight[]>('remove_preflight', { projectId, worktreeId, deleteBranch }),

  removeWorktree: (args: {
    projectId: string;
    worktreeId: string;
    deleteBranch: boolean;
    force: boolean;
    acknowledged: string[];
  }) => invoke<RemoveOutcome>('remove_worktree', args),

  /** Re-run setup: the retry remedy, and the way to adopt an externally-made worktree. */
  runSetup: (args: {
    projectId: string;
    worktreeId: string;
    extraArgs: string[];
    rows: number;
    cols: number;
  }) => invoke<SetupResult>('run_setup', args),

  /** Start one of the project's declared actions. Returns the session id to attach to. */
  runAction: (args: {
    projectId: string;
    worktreeId: string;
    actionId: string;
    rows: number;
    cols: number;
  }) => invoke<string>('run_action', args),

  // ── pty session control ──
  ptyWrite: (session: string, dataBase64: string) =>
    invoke<void>('pty_write', { session, dataBase64 }),
  ptyResize: (session: string, rows: number, cols: number) =>
    invoke<void>('pty_resize', { session, rows, cols }),
  ptyKill: (session: string) => invoke<void>('pty_kill', { session }),

  // ── trust ──
  /** Binds approval to the file's current contents; a later edit re-arms the prompt. */
  setConfigTrust: (path: string, approve: boolean) =>
    invoke<Project[]>('set_config_trust', { path, approve }),

  // ── preferences ──
  getPref: (key: string) => invoke<string | null>('get_pref', { key }),
  setPref: (key: string, value: string) => invoke<void>('set_pref', { key, value }),
  /** Palettes the user declared in `[ui.palettes]`. The built-in six are not included. */
  listPalettes: () => invoke<Palette[]>('list_palettes'),

  // ── diagnostics ──
  doctor: () => invoke<Doctor>('doctor'),

  /**
   * Fetch one withheld environment value. Read fresh from disk, never cached, and only
   * the key asked for.
   */
  revealEnvValue: (projectId: string, worktreeId: string, key: string) =>
    invoke<string>('reveal_env_value', { projectId, worktreeId, key }),

  /** Opens an http/https URL. The scheme is validated in Rust — see `open_url`. */
  openUrl: (url: string) => invoke<void>('open_url', { url }),

  // ── open in ──
  /**
   * Every tool wtm can open a worktree in, resolved against this machine.
   *
   * Returns the whole catalogue, including what is not installed, so the picker can show
   * a greyed row explaining why rather than silently omitting it. Nothing is cached in
   * Rust, so calling this again picks up an editor installed since the app started.
   */
  listOpeners: () => invoke<Openers>('list_openers'),

  /** Hands the worktree's directory to one of them. Never blocks on the app it starts. */
  openIn: (projectId: string, worktreeId: string, openerId: string) =>
    invoke<void>('open_in', { projectId, worktreeId, openerId }),
};

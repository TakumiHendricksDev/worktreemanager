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
  AgentOption,
  AgentSession,
  ApprovalAnswer,
  Capability,
  CreateOutcome,
  Doctor,
  Form,
  Openers,
  Palette,
  Preflight,
  Preview,
  Project,
  Registered,
  Resumable,
  RemoveOutcome,
  SetupResult,
  TerminalSession,
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

  // ── the terminal dock ──
  /**
   * Open, or re-attach to, the worktree's own interactive shell. Returns the session id.
   *
   * Idempotent per worktree: a second call while the shell is running hands back the same
   * session rather than a second login shell in the same directory. Unlike setup and the
   * declared actions, nothing decides when this ends but the user — the session lives until it
   * is killed or the app quits. The size is a guess the caller corrects as soon as the pane has
   * measured itself; see `Terminal.svelte`.
   */
  openTerminal: (args: {
    projectId: string;
    worktreeId: string;
    rows: number;
    cols: number;
  }) => invoke<string>('open_terminal', args),

  /**
   * Which worktrees already have a live shell. Every project's, not just the active one.
   *
   * Call on start: a reload loses this side's pane-to-session map while the shells keep
   * running, and without this they are unreachable until the app quits. It does not restore a
   * transcript — nothing is buffered outside the pane that received it.
   */
  listTerminals: () => invoke<TerminalSession[]>('list_terminals'),

  /** Kills a worktree's shell and forgets it. Restart is this, then `openTerminal`. */
  closeTerminal: (worktreeId: string) => invoke<void>('close_terminal', { worktreeId }),

  // ── pty session control ──
  ptyWrite: (session: string, dataBase64: string) =>
    invoke<void>('pty_write', { session, dataBase64 }),
  ptyResize: (session: string, rows: number, cols: number) =>
    invoke<void>('pty_resize', { session, rows, cols }),
  ptyKill: (session: string) => invoke<void>('pty_kill', { session }),

  // ── agent sessions ──
  /**
   * Every agent this build can drive, and whether this machine can.
   *
   * Includes what is not installed, with the reason, so the launcher can show a greyed row
   * explaining why. Nothing is cached in Rust, so a CLI installed since launch shows up.
   */
  listAgents: () => invoke<AgentOption[]>('list_agents'),

  /**
   * Start a session in a worktree. Returns the session id to attach to.
   *
   * Returns as soon as the CLI is running, not when it is ready — the handshake is a network
   * round trip and announces itself with `agent:ready`. Deliberately **not** idempotent per
   * worktree, unlike `openTerminal`: asking twice starts two sessions, which is the feature.
   */
  openAgentSession: (args: {
    projectId: string;
    worktreeId: string;
    agentId: string;
    /**
     * What the session asks for beyond which agent and where. Send `{}` for the provider's own
     * choices; `resume` picks up a conversation by the id its provider knows it by.
     */
    options?: {
      model?: string | null;
      effort?: string | null;
      mode?: string | null;
      resume?: string | null;
    };
  }) => invoke<string>('open_agent_session', { options: {}, ...args }),

  /**
   * Conversations that can be picked up again in this worktree, newest first.
   *
   * Excludes anything already running: offering to resume a session that is on screen would hand the
   * CLI two clients for one thread.
   */
  listResumable: (worktreeId: string) =>
    invoke<Resumable[]>('list_resumable', { worktreeId }),

  /**
   * Stop offering a conversation.
   *
   * Distinct from closing a pane, which keeps the entry — closing is how you tidy the screen, and the
   * commonest thing anyone wants next is it back. This is the explicit discard.
   */
  forgetSession: (provider: string, providerSession: string) =>
    invoke<void>('forget_session', { provider, providerSession }),

  /**
   * What an agent can do on this machine: its models and each one's effort ladder.
   *
   * Not cached in Rust. For Codex this spawns a throwaway app server and asks, which takes a second
   * or two, so callers should fetch it once when a picker is first opened rather than per render.
   */
  agentCapability: (agentId: string) => invoke<Capability>('agent_capability', { agentId }),

  /** Send one turn. Queued by the provider if the handshake has not finished yet. */
  sendTurn: (session: string, text: string) => invoke<void>('send_turn', { session, text }),

  /**
   * Answer an outstanding approval.
   *
   * The first answer wins and a second for the same id succeeds silently — the provider removes the
   * request when it replies, so two panes or a click racing a keystroke cannot both answer. The card
   * collapses on the `approval_resolved` event either way.
   */
  answerApproval: (session: string, requestId: string, answer: ApprovalAnswer) =>
    invoke<void>('answer_approval', { session, requestId, answer }),

  /** Ask the session to stop the turn it is running. */
  interruptTurn: (session: string) => invoke<void>('interrupt_turn', { session }),

  /**
   * Which agent sessions are live, across every project.
   *
   * Call on start: a reload loses this side's pane-to-session map while the CLIs keep running,
   * and without this they are unreachable until the app quits. It does not restore a transcript.
   */
  listAgentSessions: () => invoke<AgentSession[]>('list_agent_sessions'),

  /** End a session and forget it. */
  closeAgentSession: (session: string) => invoke<void>('close_agent_session', { session }),

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

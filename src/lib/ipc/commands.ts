/**
 * The only file in the frontend that calls `invoke`.
 *
 * Keeping it to one place is what makes the IPC surface greppable, mockable, and
 * type-safe in exactly one spot — a component that wants data calls `commands.*` and
 * cannot invent a command name or get its arguments wrong silently.
 */

import { invoke } from '@tauri-apps/api/core';
import type { ShellRun, ShellRunRequest, RunShell } from '../shell-script';

import type {
  AccountUsage,
  DictationStatus,
  DatabaseColumn,
  DatabaseConnection,
  DatabaseRelation,
  DatabaseSchema,
  DatabaseSession,
  Action,
  AgentAttachment,
  AgentExchange,
  AgentOption,
  AgentSession,
  BackgroundTask,
  Brief,
  BrowserAvailability,
  BrowserBounds,
  BrowserComment,
  BrowserHistoryAction,
  BrowserView,
  ApprovalAnswer,
  Capability,
  CodeChanges,
  CodeComment,
  CodeEntry,
  CodeHunk,
  CodeFile,
  CodeSearch,
  CodeSearchOptions,
  CodeStat,
  CodeSymbol,
  CodeTree,
  NewCodeComment,
  CreateOutcome,
  Doctor,
  Form,
  HomeJob,
  Openers,
  Palette,
  Preflight,
  Preview,
  Project,
  Registered,
  Resumable,
  RemoveOutcome,
  SeqEvent,
  SetupResult,
  SidebarLayout,
  TerminalReplay,
  TerminalSession,
  TablePageRequest,
  PaneWindow,
  QueryResult,
  UpdateOutcome,
  UpdateStatus,
  Worktree,
} from './types';

export const commands = {
  shellRunTarget: (projectId: string, worktreeId: string) =>
    invoke<string>('shell_run_target', { projectId, worktreeId }),
  listRunShells: (projectId: string, worktreeId: string) =>
    invoke<RunShell[]>('list_run_shells', { projectId, worktreeId }),
  prepareShellRun: (request: ShellRunRequest) =>
    invoke<ShellRun>('prepare_shell_run', { request }),
  listShellRuns: () => invoke<ShellRun[]>('list_shell_runs'),
  approveShellRun: (runId: string) => invoke<ShellRun>('approve_shell_run', { runId }),
  admitShellRun: (runId: string, rows: number, cols: number) =>
    invoke<ShellRun>('admit_shell_run', { runId, rows, cols }),
  denyShellRun: (runId: string) => invoke<void>('deny_shell_run', { runId }),
  cancelShellRun: (runId: string) => invoke<void>('cancel_shell_run', { runId }),
  focusRunShell: (session: string) => invoke<void>('focus_run_shell', { session }),
  setHomeShellGrant: (
    home: string,
    projectId: string,
    worktreeId: string,
    allow: boolean,
  ) => invoke<void>('set_home_shell_grant', { home, projectId, worktreeId, allow }),
  popupNativeMenu: (rid: number, kind: 'menu' | 'submenu', at?: { x: number; y: number }) =>
    invoke<void>('popup_native_menu', { rid, kind, at }),
  // ── projects ──
  listProjects: () => invoke<Project[]>('list_projects'),
  /** Accepts any path inside a repository. Returns the resolved id, not just the list. */
  registerProject: (path: string) => invoke<Registered>('register_project', { path }),
  /** Takes the project's root — the store removes an exact key, not a prefix. */
  unregisterProject: (path: string) => invoke<Project[]>('unregister_project', { path }),

  // ── worktrees ──
  listWorktrees: (projectId: string) => invoke<Worktree[]>('list_worktrees', { projectId }),
  /** Order, groups, folds and stars. Read when a project opens — see the Rust doc comment. */
  sidebarLayout: (projectId: string) =>
    invoke<SidebarLayout>('sidebar_layout', { projectId }),
  /** Persists to `~/.config/wtm/config.toml` and answers with the layout as normalized. */
  setSidebarLayout: (projectId: string, layout: SidebarLayout) =>
    invoke<SidebarLayout>('set_sidebar_layout', { projectId, layout }),

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
   * Open an interactive shell in a worktree. Returns the session id.
   *
   * **Every call opens one.** This used to be idempotent per worktree; a worktree can now hold
   * several shells, so deciding whether to reuse one is the caller's business — see
   * `sessions.focusOrOpenShell`, which is what ⌘J goes through. Unlike setup and the declared
   * actions, nothing decides when this ends but the user: the session lives until it is killed or
   * the app quits. The size is a guess the caller corrects as soon as the pane has measured
   * itself; see `Terminal.svelte`.
   */
  openTerminal: (args: {
    projectId: string;
    worktreeId: string;
    rows: number;
    cols: number;
  }) => invoke<string>('open_terminal', args),

  /**
   * Every live shell, one row each. Every project's, not just the active one.
   *
   * Call on start: a reload loses this side's pane-to-session map while the shells keep
   * running, and without this they are unreachable until the app quits. It carries no
   * transcript; a terminal attaching asks `terminalReplay` for that.
   */
  listTerminals: () => invoke<TerminalSession[]>('list_terminals'),

  /**
   * What a dock shell has printed that Rust still keeps, or `null` for none.
   *
   * How a terminal attaches to a shell it did not see start — in a pane window, on the way back
   * from one, after a reload. Live chunks numbered at or below `through` are already in it.
   */
  terminalReplay: (session: string) =>
    invoke<TerminalReplay | null>('terminal_replay', { session }),

  /**
   * Kills one shell and forgets it. Restart is this, then `openTerminal`.
   *
   * By session, not by worktree: a worktree may have several shells, and one of them may be
   * running a dev server.
   */
  closeTerminal: (session: string) => invoke<void>('close_terminal', { session }),

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
   *
   * The project decides what `offered` says — a repository can decline an agent, and this list did
   * not used to know it, so the launcher offered agents whose spawn would be refused. Omit it for
   * the startup call, before anything is selected.
   */
  listAgents: (projectId?: string | null) =>
    invoke<AgentOption[]>('list_agents', { projectId: projectId ?? null }),

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
      /** Claude's high-speed mode. Ignored by a provider that has none. */
      fast?: boolean | null;
      resume?: string | null;
      /**
       * For a pane restored from the last run: the Home session that opened it, as that Home
       * conversation is running now, so Home may still close it. Ignored unless it names one.
       */
      openedBy?: string | null;
      /** For a pane restored from the last run: the user had written to it, so Home may not. */
      typed?: boolean | null;
    };
  }) => invoke<string>('open_agent_session', { options: {}, ...args }),

  /**
   * Start a Home session: an agent outside every worktree, whose tools reach the sessions inside
   * them. `resume` picks up a past Home conversation, listed by `listResumable(HOME)`.
   */
  openHomeSession: (args: {
    agentId: string;
    options?: {
      model?: string | null;
      effort?: string | null;
      mode?: string | null;
      fast?: boolean | null;
      resume?: string | null;
    };
  }) => invoke<string>('open_home_session', { options: {}, ...args }),

  /** Worktrees the Home agent is creating or created lately, oldest first. */
  homeJobs: () => invoke<HomeJob[]>('home_jobs'),

  /**
   * Tell a restored Home conversation, once, that the quit interrupted the work it had out. Each
   * target is a session it had sent work to, with `session` null when that one is not running.
   */
  homeInterrupted: (
    home: string,
    targets: {
      session: string | null;
      provider: string;
      project: string;
      worktree: string;
    }[],
  ) => invoke<void>('home_interrupted', { home, targets }),

  /** Fork a live conversation for one ephemeral `/btw` question. */
  openAgentSideSession: (args: {
    parentSession: string;
    options?: {
      model?: string | null;
      effort?: string | null;
      mode?: string | null;
      /** Claude's high-speed mode. Ignored by a provider that has none. */
      fast?: boolean | null;
    };
  }) => invoke<string>('open_agent_side_session', { options: {}, ...args }),

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

  /**
   * Every provider's usage record as Rust holds it, without asking any provider.
   *
   * What a window seeds the usage store with, so a reload or a popped-out pane starts from what
   * the app already knows. Live changes arrive as `usage:limits`.
   */
  usageLimits: () => invoke<AccountUsage[]>('usage_limits'),

  /**
   * Ask one provider directly for what it will say, and get its record back.
   *
   * Starts a short-lived CLI, so it takes from half a second (Claude, Cursor) to a few (Codex, which
   * starts its MCP servers first). Call it when the usage view opens, not per render. A failed ask
   * still resolves, with `error` set on the record.
   */
  refreshUsageLimits: (agentId: string) =>
    invoke<AccountUsage>('refresh_usage_limits', { agentId }),

  /** Send one turn. Queued by the provider if the handshake has not finished yet. */
  sendTurn: (session: string, text: string, attachments: AgentAttachment[] = []) =>
    invoke<void>('send_turn', { session, text, attachments }),

  /**
   * Hand a message to the turn that is running rather than the next one.
   *
   * The provider echoes it when it takes it, not now — see `Protocol::steer` — and a session that
   * turns out to have finished in the meantime simply starts a turn with it.
   */
  steerTurn: (session: string, text: string, attachments: AgentAttachment[] = []) =>
    invoke<void>('steer_turn', { session, text, attachments }),

  /** Read a file explicitly picked or dropped into the composer. */
  prepareAgentAttachment: (path: string) =>
    invoke<AgentAttachment>('prepare_agent_attachment', { path }),

  /** Stage bytes pasted from the clipboard and return the same normalized attachment shape. */
  stageAgentAttachment: (name: string, mime: string, dataBase64: string) =>
    invoke<AgentAttachment>('stage_agent_attachment', { name, mime, dataBase64 }),

  /**
   * Change a running session's model, effort, mode or fast mode. `null` leaves one alone. The UI
   * sends effort only to Codex; Claude's effort remains an explicit restart setting.
   *
   * `fast` is Claude's alone and goes the other way: it applies live, and whether it *took* comes
   * back on the next turn rather than from this call. See the Rust side's `fast_mode_notice`.
   */
  configureSession: (
    session: string,
    model: string | null,
    effort: string | null,
    mode: string | null,
    fast: boolean | null,
  ) => invoke<void>('configure_session', { session, model, effort, mode, fast }),

  /** Whether dictation can be offered: SoX, curl, a secret store, and a stored key. */
  dictationStatus: () => invoke<DictationStatus>('dictation_status'),

  /**
   * Store the transcription key. One-way — there is no command that reads it back.
   *
   * An empty string clears it. See `DictationStatus.keySet`, which is all the frontend ever
   * learns about a stored key.
   */
  setDictationKey: (key: string) => invoke<void>('set_dictation_key', { key }),

  /** Begin recording from the microphone. */
  startDictation: () => invoke<void>('start_dictation'),

  /** Stop recording and transcribe. Rejects with a human-readable reason. */
  stopDictation: () => invoke<string>('stop_dictation'),

  // ── updates ──
  /**
   * Ask GitHub whether a newer release exists. `null` when an automatic check is turned off or
   * this is a debug build; a `manual` check always runs.
   */
  checkForUpdate: (manual: boolean) =>
    invoke<UpdateStatus | null>('check_for_update', { manual }),
  /**
   * `brew update`, confirm Homebrew sees the new version, and download it — while the app is still
   * open, so a failure is an error here rather than after quitting. Resolves to the version
   * Homebrew will install.
   */
  prepareUpdate: () => invoke<string>('prepare_update'),
  /** Leave the upgrade helper behind and quit. On success this never resolves: the app is gone. */
  installUpdate: (version: string) => invoke<void>('install_update', { version }),
  /** How the previous run's update went. Consumed by the first call; `null` after that. */
  takeUpdateOutcome: () => invoke<UpdateOutcome | null>('take_update_outcome'),

  /**
   * Every file in a worktree worth offering in the composer's `@` list.
   *
   * `git ls-files`, so `.gitignore` is honoured — a plain walk would offer `node_modules`, which is
   * enough paths on its own to make a typeahead feel broken. Paths are relative to the worktree.
   * Worth caching per worktree: this shells out, and the answer changes only when files do.
   */
  listWorktreeFiles: (worktreeId: string) =>
    invoke<string[]>('list_worktree_files', { worktreeId }),

  /**
   * Answer an outstanding approval.
   *
   * The first answer wins and a second for the same id succeeds silently — the provider removes the
   * request when it replies, so two panes or a click racing a keystroke cannot both answer. The card
   * collapses on the `approval_resolved` event either way.
   */
  answerApproval: (session: string, requestId: string, answer: ApprovalAnswer) =>
    invoke<void>('answer_approval', { session, requestId, answer }),

  /**
   * Store a plan, so it outlives the session that wrote it.
   *
   * Called when a plan approval is allowed — the moment a plan stops moving. Nothing is written into
   * the worktree; this goes to `~/.config/wtm/plans/`.
   */
  saveBrief: (args: {
    projectId: string;
    worktreeId: string;
    provider: string;
    markdown: string;
    model?: string | null;
    providerSession?: string | null;
    providerPath?: string | null;
  }) => invoke<string>('save_brief', args),

  listBriefs: (projectId: string, worktreeId: string) =>
    invoke<Brief[]>('list_briefs', { projectId, worktreeId }),

  removeBrief: (projectId: string, id: string) =>
    invoke<void>('remove_brief', { projectId, id }),

  /**
   * Background agents running in a worktree. Claude Code only — Codex has no equivalent roster.
   *
   * There is no event when one finishes, so this is read on demand and on window focus, the same
   * triggers `listWorktrees` uses. Polling is banned.
   */
  listBackgroundTasks: (worktreeId: string) =>
    invoke<BackgroundTask[]>('list_background_tasks', { worktreeId }),

  /** Ask the session to stop the turn it is running. */
  interruptTurn: (session: string) => invoke<void>('interrupt_turn', { session }),

  /**
   * Which agent sessions are live, across every project.
   *
   * Call on start: a reload loses this side's pane-to-session map while the CLIs keep running,
   * and without this they are unreachable until the app quits. It does not restore a transcript.
   */
  listAgentSessions: () => invoke<AgentSession[]>('list_agent_sessions'),

  /** Everything a live session has already said, so a re-attached pane is not blank. */
  agentReplay: (session: string) => invoke<SeqEvent[]>('agent_replay', { session }),

  /**
   * Whether this window should bring the last run's sessions back. True once per run of the app,
   * so a reload, whose sessions are still running, answers false.
   */
  claimLaunchRestore: () => invoke<boolean>('claim_launch_restore'),

  /**
   * Say in a restored session's transcript that the quit stopped its turn: the tool rows it left
   * open are closed, and a notice follows. `waiting` when it was waiting on the user.
   */
  markInterruptedByQuit: (session: string, waiting: boolean) =>
    invoke<void>('mark_interrupted_by_quit', { session, waiting }),

  /** What agents have recently said to each other, oldest first. Home draws its wires from it. */
  agentMessages: () => invoke<AgentExchange[]>('agent_messages'),

  /** End a session and forget it. */
  closeAgentSession: (session: string) => invoke<void>('close_agent_session', { session }),

  // ── trust ──
  /** Binds approval to the file's current contents; a later edit re-arms the prompt. */
  setConfigTrust: (path: string, approve: boolean) =>
    invoke<Project[]>('set_config_trust', { path, approve }),

  // ── preferences ──
  getPref: (key: string) => invoke<string | null>('get_pref', { key }),
  setPref: (key: string, value: string) => invoke<void>('set_pref', { key, value }),
  /** Palettes the user declared in `[ui.palettes]`. The built-in nine are not included. */
  listPalettes: () => invoke<Palette[]>('list_palettes'),

  // ── diagnostics ──
  doctor: () => invoke<Doctor>('doctor'),

  /**
   * Fetch one withheld environment value. Read fresh from disk, never cached, and only
   * the key asked for.
   */
  revealEnvValue: (projectId: string, worktreeId: string, key: string) =>
    invoke<string>('reveal_env_value', { projectId, worktreeId, key }),

  // ── databases ──
  listDatabaseConnections: (projectId: string, worktreeId: string) =>
    invoke<DatabaseConnection[]>('list_database_connections', { projectId, worktreeId }),
  connectDatabase: (projectId: string, worktreeId: string, profileId: string) =>
    invoke<DatabaseSession>('connect_database', { projectId, worktreeId, profileId }),
  disconnectDatabase: (session: string) => invoke<void>('disconnect_database', { session }),
  databaseSchemas: (session: string) =>
    invoke<DatabaseSchema[]>('database_schemas', { session }),
  databaseRelations: (session: string, schema: string) =>
    invoke<DatabaseRelation[]>('database_relations', { session, schema }),
  databaseColumns: (session: string, schema: string, relation: string) =>
    invoke<DatabaseColumn[]>('database_columns', { session, schema, relation }),
  /**
   * `readOnly` runs one statement in a transaction the server keeps read-only — for SQL the user
   * did not type into the console, such as a statement from an agent's reply.
   */
  runDatabaseQuery: (session: string, sql: string, readOnly = false, maxRows = 500) =>
    invoke<QueryResult>('run_database_query', { session, sql, maxRows, readOnly }),
  databaseTablePage: (session: string, request: TablePageRequest) =>
    invoke<QueryResult>('database_table_page', { session, request }),
  cancelDatabaseQuery: (session: string) =>
    invoke<void>('cancel_database_query', { session }),

  // ── the Code tab ──
  /** Every file git lists in the worktree, the ignored paths, and which ones are not plain. */
  codeTree: (projectId: string, worktreeId: string) =>
    invoke<CodeTree>('code_tree', { projectId, worktreeId }),
  /** One level of a directory git did not list — ignored, a submodule, or a link. */
  codeListDir: (projectId: string, worktreeId: string, dir: string) =>
    invoke<CodeEntry[]>('code_list_dir', { projectId, worktreeId, dir }),
  /** One file's text, up to five mebibytes; a binary file comes back without any. */
  codeReadFile: (projectId: string, worktreeId: string, path: string) =>
    invoke<CodeFile>('code_read_file', { projectId, worktreeId, path }),
  /** Find in Files. Starting one stops the one before, which then rejects as `cancelled`. */
  codeSearch: (
    projectId: string,
    worktreeId: string,
    query: string,
    options: CodeSearchOptions,
  ) => invoke<CodeSearch>('code_search', { projectId, worktreeId, query, options }),
  /** What the worktree changed, against its base branch's merge base or against `HEAD`. */
  codeChanges: (projectId: string, worktreeId: string, scope: 'branch' | 'uncommitted') =>
    invoke<CodeChanges>('code_changes', { projectId, worktreeId, scope }),
  /** One file's zero-context hunks against `rev`, which must be what `codeChanges` returned. */
  codeFileDiff: (
    projectId: string,
    worktreeId: string,
    path: string,
    rev: string,
    from: string | null,
  ) => invoke<CodeHunk[]>('code_file_diff', { projectId, worktreeId, path, rev, from }),
  /** A file's content at `rev`, or null when it did not exist there. */
  codeBaseVersion: (projectId: string, worktreeId: string, path: string, rev: string) =>
    invoke<string | null>('code_base_version', { projectId, worktreeId, path, rev }),
  // Comments on lines. Each answers with the worktree's whole list, and announces it as well
  // on `code:comments`, which is what every window draws from.
  codeListComments: (worktreeId: string) =>
    invoke<CodeComment[]>('code_list_comments', { worktreeId }),
  codeAddComment: (worktreeId: string, comment: NewCodeComment) =>
    invoke<CodeComment[]>('code_add_comment', { worktreeId, comment }),
  codeUpdateComment: (worktreeId: string, id: number, text: string) =>
    invoke<CodeComment[]>('code_update_comment', { worktreeId, id, text }),
  codeResolveComment: (worktreeId: string, id: number, resolved: boolean) =>
    invoke<CodeComment[]>('code_resolve_comment', { worktreeId, id, resolved }),
  codeRemoveComment: (worktreeId: string, id: number) =>
    invoke<CodeComment[]>('code_remove_comment', { worktreeId, id }),
  codeClearComments: (worktreeId: string, resolvedOnly: boolean) =>
    invoke<CodeComment[]>('code_clear_comments', { worktreeId, resolvedOnly }),
  codeMarkCommentsSent: (worktreeId: string, ids: number[], to: string) =>
    invoke<CodeComment[]>('code_mark_comments_sent', { worktreeId, ids, to }),
  /**
   * Go to Class (`typesOnly`) or Go to Symbol. `refresh` re-reads what changed since the index was
   * built, which the palette asks for once, when it opens.
   */
  codeSymbols: (
    projectId: string,
    worktreeId: string,
    query: string,
    typesOnly: boolean,
    refresh: boolean,
  ) =>
    invoke<CodeSymbol[]>('code_symbols', {
      projectId,
      worktreeId,
      query,
      typesOnly,
      refresh,
    }),
  /** Every definition named exactly this — what ⌘-click on a name goes to. */
  codeDefinitions: (projectId: string, worktreeId: string, name: string) =>
    invoke<CodeSymbol[]>('code_definitions', { projectId, worktreeId, name }),
  /** When each open file last changed, so a refresh re-reads only what did. */
  codeStat: (projectId: string, worktreeId: string, paths: string[]) =>
    invoke<CodeStat[]>('code_stat', { projectId, worktreeId, paths }),

  /** Opens an http/https URL. The scheme is validated in Rust — see `open_url`. */
  openUrl: (url: string) => invoke<void>('open_url', { url }),

  // ── browser panes ──
  /**
   * Put a real webview over a tile. Born hidden; `browserSetBounds` is what shows it.
   *
   * Refused at the caps (four per worktree, eight in all) with `kind: 'browserCap'`, and for a
   * URL that is not http(s) with `kind: 'badUrl'` — the same rule `openUrl` applies.
   */
  openBrowser: (args: { projectId: string; worktreeId: string; url?: string | null }) =>
    invoke<BrowserView>('open_browser', args),
  closeBrowser: (id: string) => invoke<void>('close_browser', { id }),
  browserNavigate: (id: string, url: string) =>
    invoke<BrowserView>('browser_navigate', { id, url }),
  browserHistory: (id: string, action: BrowserHistoryAction) =>
    invoke<void>('browser_history', { id, action }),
  /**
   * Where the tile is, or `null` to hide the browser.
   *
   * The native view floats above every DOM element, so the pane sends this on every geometry
   * change and `null` whenever anything would need to paint over it — see `BrowserPane.svelte`.
   */
  browserSetBounds: (id: string, bounds: BrowserBounds | null) =>
    invoke<void>('browser_set_bounds', { id, bounds }),
  browserSetAgentAccess: (id: string, enabled: boolean) =>
    invoke<BrowserView>('browser_set_agent_access', { id, enabled }),
  browserFocus: (id: string) => invoke<void>('browser_focus', { id }),
  browserZoom: (id: string, factor: number) => invoke<void>('browser_zoom', { id, factor }),
  /** Every live browser, for adopting after a webview reload. */
  listBrowsers: (worktreeId?: string) =>
    invoke<BrowserView[]>('list_browsers', { worktreeId: worktreeId ?? null }),
  browserAvailable: () => invoke<BrowserAvailability>('browser_available'),
  /** Comment mode: a click in the page picks an element instead of acting on it. */
  browserSetCommentMode: (id: string, enabled: boolean) =>
    invoke<BrowserView>('browser_set_comment_mode', { id, enabled }),
  browserListComments: (id: string) =>
    invoke<BrowserComment[]>('browser_list_comments', { id }),
  browserUpdateComment: (id: string, commentId: number, text: string) =>
    invoke<BrowserComment[]>('browser_update_comment', { id, commentId, text }),
  browserRemoveComment: (id: string, commentId: number) =>
    invoke<BrowserComment[]>('browser_remove_comment', { id, commentId }),
  browserResolveComment: (id: string, commentId: number, resolved: boolean) =>
    invoke<BrowserComment[]>('browser_resolve_comment', { id, commentId, resolved }),
  /**
   * A PNG of the page as shown, base64. Refused while the pane is hidden — there is nothing to
   * capture — so the pane asks *before* it hides.
   */
  browserSnapshotPng: (id: string) => invoke<string>('browser_snapshot_png', { id }),
  /** The app's colours, for the pins and popover the runtime draws inside the page. */
  browserSetTheme: (id: string, tokens: Record<string, string>) =>
    invoke<void>('browser_set_theme', { id, tokens }),
  browserOpenDevtools: (id: string) => invoke<void>('browser_open_devtools', { id }),

  // ── pane windows ──
  /**
   * Move a pane into a window of its own, opening over `rect` — where its tile was, in this
   * window's CSS pixels. `state` is handed to the new window as it starts. Main window only.
   */
  popOutPane: (args: {
    paneId: string;
    title: string;
    state: unknown;
    rect: BrowserBounds;
  }) => invoke<void>('pop_out_pane', args),
  /** The pane this window holds, for a pane window starting up. `null` in the main window. */
  paneWindowState: () => invoke<PaneWindow | null>('pane_window_state'),
  /** A pane window's latest record of its pane, passed on to the main window. */
  syncPaneWindow: (state: unknown) => invoke<void>('sync_pane_window', { state }),
  /** Put a pane back, by closing its window — which is also what the traffic light does. */
  closePaneWindow: (paneId: string) => invoke<void>('close_pane_window', { paneId }),
  /** Bring a pane's window forward, or the main window when no pane is named. */
  focusPaneWindow: (paneId: string | null) => invoke<void>('focus_pane_window', { paneId }),
  /** Every pane window, for a main window that has reloaded. */
  listPaneWindows: () => invoke<PaneWindow[]>('list_pane_windows'),

  // ── notifications ──
  /**
   * Post a macOS notification whose click navigates back to the pane it is about.
   *
   * Through Rust rather than the notification plugin's JS API, because the plugin cannot
   * carry a payload a click delivers back — see `notifier.rs`. Rejects when the OS is
   * refusing delivery, which is the signal `attention.blocked` keys off.
   */
  postNotification: (args: {
    title: string;
    body: string;
    projectId: string;
    worktreeId: string;
    paneId: string;
  }) => invoke<void>('post_notification', args),

  /** What the OS says about delivering notifications. */
  notificationPermission: () =>
    invoke<'granted' | 'denied' | 'prompt'>('notification_permission'),

  /**
   * Ask the OS for permission. Resolves when the user answers the prompt, and rejects with kind
   * `notifications_not_allowed` at once when macOS will not ask an unsigned build at all.
   */
  requestNotificationPermission: () => invoke<boolean>('request_notification_permission'),

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

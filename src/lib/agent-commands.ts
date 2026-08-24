import type { AgentSkill } from './ipc/types';

/**
 * Provider-owned commands that are not included in the machine-readable capability handshake.
 *
 * Claude reports installed skills and commands only after its first turn; Codex app-server reports
 * skills but leaves TUI commands to its client. Keeping the documented built-ins here makes `/`
 * useful from the first prompt. Live discoveries are merged over this list, so repository skills,
 * plugins and future provider additions still win without an app release.
 */
const CODEX: ReadonlyArray<readonly [string, string]> = [
  ['permissions', 'Change approval and sandbox permissions'],
  ['ide', 'Include current editor context'],
  ['keymap', 'Inspect or change keyboard shortcuts'],
  ['vim', 'Toggle Vim composer mode'],
  ['setup-default-sandbox', 'Set up the elevated Windows sandbox'],
  ['sandbox-add-read-dir', 'Grant Windows sandbox read access to a directory'],
  ['agent', 'Switch between agent threads'],
  ['subagents', 'Switch between agent threads'],
  ['apps', 'Browse connected apps'],
  ['plugins', 'Browse and manage plugins'],
  ['hooks', 'View and manage lifecycle hooks'],
  ['clear', 'Clear this conversation and start a fresh chat'],
  ['rename', 'Rename the current chat'],
  ['archive', 'Archive the current session'],
  ['delete', 'Permanently delete the current session'],
  ['compact', 'Summarize the conversation to free context'],
  ['copy', 'Copy the latest completed response'],
  ['diff', 'Review working-tree changes'],
  ['exit', 'Exit the session'],
  ['experimental', 'Toggle experimental features'],
  ['approve', 'Retry an auto-review denial'],
  ['memories', 'Configure memory use and generation'],
  ['skills', 'Browse and invoke skills'],
  ['import', 'Import another coding agent setup'],
  ['feedback', 'Send feedback and diagnostics'],
  ['init', 'Create an AGENTS.md scaffold'],
  ['logout', 'Sign out of Codex'],
  ['mcp', 'List connected MCP tools'],
  ['mention', 'Attach a file or folder'],
  ['model', 'Choose model and reasoning effort'],
  ['fast', 'Toggle the Fast service tier'],
  ['plan', 'Switch to plan mode'],
  ['goal', 'Set or inspect the persistent task goal'],
  ['personality', 'Choose a response style'],
  ['ps', 'Inspect background terminals'],
  ['stop', 'Stop background terminals'],
  ['fork', 'Fork the current chat'],
  ['app', 'Continue in the desktop app'],
  ['side', 'Ask a side question outside the main transcript'],
  ['btw', 'Ask a side question outside the main transcript'],
  ['raw', 'Toggle raw scrollback mode'],
  ['resume', 'Resume a saved chat'],
  ['new', 'Start a new chat'],
  ['quit', 'Exit the session'],
  ['review', 'Review the working tree'],
  ['status', 'Show session settings and context usage'],
  ['usage', 'Show account usage and limits'],
  ['debug-config', 'Inspect configuration layers'],
  ['statusline', 'Configure status-line fields'],
  ['title', 'Configure terminal-title fields'],
  ['theme', 'Choose a syntax theme'],
  ['pets', 'Choose a terminal pet'],
  ['pet', 'Choose a terminal pet'],
  ['context', 'Show live context-window usage'],
];

const CLAUDE: ReadonlyArray<readonly [string, string]> = [
  ['add-dir', 'Add another working directory'],
  ['advisor', 'Configure the second-model advisor'],
  ['agents', 'Manage subagent configuration'],
  ['autocompact', 'Configure automatic context compaction'],
  ['autofix-pr', 'Watch and repair CI or review failures'],
  ['background', 'Detach this session into the background'],
  ['bg', 'Detach this session into the background'],
  ['batch', 'Split a large change across worktrees'],
  ['branch', 'Branch this conversation'],
  ['btw', 'Ask a side question outside the main transcript'],
  ['bug', 'Report a bug with optional session context'],
  ['cd', 'Change this session’s working directory'],
  ['chrome', 'Configure Claude in Chrome'],
  ['claude-api', 'Open Claude API setup and migration tools'],
  ['clear', 'Clear this conversation and start fresh'],
  ['code-review', 'Review a diff, branch, or pull request'],
  ['review', 'Review a diff, branch, or pull request'],
  ['color', 'Set the session color'],
  ['compact', 'Summarize the conversation to free context'],
  ['config', 'Open Claude Code settings'],
  ['context', 'Visualize what is using the context window'],
  ['copy', 'Copy a recent assistant response'],
  ['cost', 'Show usage and cost'],
  ['dataviz', 'Design a chart or dashboard'],
  ['debug', 'Enable logs and diagnose a problem'],
  ['deep-research', 'Research a question with parallel web work'],
  ['design-login', 'Authorize Claude Design'],
  ['design-sync', 'Sync a React design system'],
  ['desktop', 'Continue in Claude Code Desktop'],
  ['app', 'Continue in Claude Code Desktop'],
  ['diff', 'Open the interactive diff viewer'],
  ['doctor', 'Diagnose and repair the installation'],
  ['effort', 'Change reasoning effort'],
  ['exit', 'Exit the session'],
  ['quit', 'Exit the session'],
  ['export', 'Export the conversation'],
  ['fast', 'Toggle fast mode'],
  ['feedback', 'Send product feedback'],
  ['fewer-permission-prompts', 'Create a safe project allowlist'],
  ['focus', 'Toggle the compact focus view'],
  ['fork', 'Copy this conversation to a background session'],
  ['goal', 'Set or clear a persistent task goal'],
  ['heapdump', 'Write a diagnostic heap snapshot'],
  ['help', 'Show available commands'],
  ['hooks', 'Inspect lifecycle hooks'],
  ['ide', 'Manage IDE integrations'],
  ['import', 'Import another coding-agent setup'],
  ['init', 'Create or improve CLAUDE.md'],
  ['insights', 'Analyze recent coding sessions'],
  ['install-github-app', 'Install the Claude GitHub App'],
  ['install-slack-app', 'Install the Claude Slack app'],
  ['keybindings', 'Open the keybindings file'],
  ['list-agents', 'List reachable agents and sessions'],
  ['login', 'Sign in to Anthropic'],
  ['logout', 'Sign out of Anthropic'],
  ['loop', 'Run a prompt repeatedly'],
  ['mcp', 'Manage MCP servers'],
  ['memory', 'Edit CLAUDE.md and auto memory'],
  ['mobile', 'Open Claude mobile setup'],
  ['model', 'Switch model'],
  ['passes', 'Share an eligible Claude Code pass'],
  ['permissions', 'Manage tool permission rules'],
  ['plan', 'Enter plan mode'],
  ['plugin', 'Manage plugins'],
  ['powerup', 'Open interactive feature lessons'],
  ['privacy-settings', 'Review privacy settings'],
  ['radio', 'Open Claude FM'],
  ['recap', 'Summarize the current session in one line'],
  ['release-notes', 'Browse release notes'],
  ['reload-plugins', 'Reload active plugins'],
  ['reload-skills', 'Rescan skill directories'],
  ['remote-control', 'Continue this session from another device'],
  ['remote-env', 'Choose a cloud-agent environment'],
  ['rename', 'Rename this session'],
  ['resume', 'Resume a saved conversation'],
  ['rewind', 'Restore or summarize from a checkpoint'],
  ['checkpoint', 'Restore or summarize from a checkpoint'],
  ['undo', 'Restore or summarize from a checkpoint'],
  ['run', 'Launch and inspect the project app'],
  ['run-skill-generator', 'Teach run and verify how to launch this project'],
  ['sandbox', 'Toggle sandbox mode'],
  ['schedule', 'Manage cloud routines'],
  ['scroll-speed', 'Adjust transcript scroll speed'],
  ['security-review', 'Review branch changes for vulnerabilities'],
  ['setup-bedrock', 'Configure Amazon Bedrock'],
  ['setup-vertex', 'Configure Google Cloud models'],
  ['simplify', 'Review changed code for cleanup opportunities'],
  ['skills', 'Browse installed skills'],
  ['stats', 'Show usage statistics'],
  ['status', 'Show version, model, account, and connectivity'],
  ['statusline', 'Configure the status line'],
  ['stickers', 'Open Claude Code sticker ordering'],
  ['stop', 'Stop a background session'],
  ['subtask', 'Spawn a forked subagent'],
  ['tasks', 'Manage background tasks and subagents'],
  ['bashes', 'Manage background tasks and subagents'],
  ['team-onboarding', 'Generate a team onboarding guide'],
  ['teleport', 'Pull a web session into this terminal'],
  ['tp', 'Pull a web session into this terminal'],
  ['terminal-setup', 'Configure terminal shortcuts'],
  ['theme', 'Change the color theme'],
  ['tui', 'Choose the terminal renderer'],
  ['ultrareview', 'Run a cloud multi-agent code review'],
  ['upgrade', 'Open plan upgrade options'],
  ['usage', 'Show usage, limits, and cost'],
  ['usage-credits', 'Configure extra usage credits'],
  ['verify', 'Build, run, and observe a change'],
  ['voice', 'Configure voice dictation'],
  ['web-setup', 'Connect GitHub for cloud sessions'],
  ['workflows', 'Inspect running workflows'],
];

const CATALOGUES: Record<string, ReadonlyArray<readonly [string, string]>> = {
  codex: CODEX,
  claude: CLAUDE,
};

/**
 * Everything this session can be asked to do by name, best first.
 *
 * # Merged field by field, not entry by entry
 *
 * This used to do `merged.set(name, command)`, which replaced the whole entry. Claude reports
 * **names only** — its init line carries a `slash_commands` array of bare strings — so the moment a
 * session said anything, every command in both lists lost its description and the `/` menu's second
 * column went blank. It was better documented before the provider spoke than after. A live entry
 * still wins every field it actually has; a `null` is not an answer.
 *
 * # Why the order is the interesting part
 *
 * The built-in table is ~110 entries and the repository's own skills are the ones somebody typed
 * `/` to reach. `suggest.ts` shows the first `LIMIT` of whatever it is handed when nothing has been
 * typed yet, so seeding the map with built-ins meant a bare `/` listed `add-dir … cost` and never
 * reached a single project skill — in a repository with fifty-five of them. Discovered entries
 * therefore come first, and the built-ins follow.
 */
export function commandsFor(provider: string, live: readonly AgentSkill[]): AgentSkill[] {
  const merged = new Map<string, AgentSkill>();
  for (const [name, description] of CATALOGUES[provider] ?? []) {
    merged.set(name, { name, description, scope: 'built-in' });
  }
  for (const command of live) {
    const known = merged.get(command.name);
    merged.set(command.name, {
      name: command.name,
      description: command.description ?? known?.description ?? null,
      scope: command.scope ?? known?.scope ?? null,
    });
  }

  const entries = [...merged.values()];
  const discovered = entries.filter((entry) => entry.scope !== 'built-in');
  const builtIn = entries.filter((entry) => entry.scope === 'built-in');
  return [...discovered, ...builtIn];
}

import catalog from '../../crates/wtm-plugins/contracts/catalog.json';
export { catalog };
export type ExtensionContext = {
  projectId: string;
  worktreeId: string;
  paneId?: string;
  session?: string;
  messageId?: string;
  messageText?: string;
  [key: string]: unknown;
};
export type ExtensionStyle = {
  id: string;
  title: string;
  target: string;
  properties: Record<string, string>;
};
export type ExtensionAction = {
  id: string;
  title: string;
  target: string;
  operation: string;
  arguments: unknown;
  replaces: string | null;
};
export type ExtensionSetting = {
  id: string;
  title: string;
  description: string;
  section: string;
  kind: 'text' | 'boolean' | 'number' | 'select';
  default: unknown;
  options: string[];
};
export type ExtensionWidget =
  | { kind: 'text' | 'markdown'; text: string }
  | { kind: 'action'; action: string }
  | { kind: 'setting'; setting: string }
  | { kind: 'group'; title: string; children: ExtensionWidget[] };
export type ExtensionContent = {
  id: string;
  title: string;
  target: string;
  position: 'before' | 'after' | 'replace';
  body: ExtensionWidget[];
};
export type ExtensionHook = {
  id: string;
  title: string;
  operation: string;
  phase: 'before' | 'after';
};
export type ExtensionContributions = {
  access: string[];
  runtime: string[];
  events: string[];
  styles: ExtensionStyle[];
  actions: ExtensionAction[];
  content: ExtensionContent[];
  settings: ExtensionSetting[];
  hooks: ExtensionHook[];
};
export type ExtensionPreferences = {
  disabled: string[];
  worktrees: string[];
  values: Record<string, Record<string, unknown>>;
};
export type Contribution<T> = {
  plugin: string;
  packageName: string;
  key: string;
  value: T;
};
export const emptyExtensions: ExtensionContributions = {
  access: [],
  runtime: [],
  events: [],
  styles: [],
  actions: [],
  content: [],
  settings: [],
  hooks: [],
};

export type WorkbenchPanel = Pick<
  import('./state/sessions.svelte').Pane,
  'id' | 'session' | 'projectId' | 'worktreeId' | 'kind' | 'working' | 'ended'
> & { title: string | null };

/** Wire payloads for the frontend-owned operations; native operations use commands.ts. */
export interface WorkbenchOperations {
  'ui.workspace.rename': {
    arguments: { name: string };
    result: { id: string; name: string; path: string };
  };
  'ui.panels.list': { arguments: Record<string, never>; result: WorkbenchPanel[] };
  'ui.panels.open': {
    arguments: { kind: 'chat' | 'shell' | 'browser'; provider?: string; url?: string };
    result: string | null | void;
  };
  'ui.panels.focus': { arguments: { paneId?: string }; result: WorkbenchPanel };
  'ui.panels.close': { arguments: { paneId?: string }; result: { closed: string } };
  'ui.panels.move': {
    arguments: { paneId?: string; target: import('./state/layout.svelte').Target };
    result: WorkbenchPanel;
  };
  'ui.panels.resize': {
    arguments: { path: string; ratio: number };
    result: { path: string; ratio: number };
  };
  'ui.chat.snapshot': {
    arguments: { paneId?: string };
    result: import('./state/chat-edit.svelte').ChatEditSnapshot &
      WorkbenchPanel & { events: import('./ipc/types').AgentEvent[] };
  };
  'ui.chat.edit': {
    arguments: { paneId?: string; messageId?: string };
    result: { opened: boolean };
  };
  'ui.chat.configure': {
    arguments: { paneId?: string; model?: string; effort?: string; fast?: boolean };
    result: WorkbenchPanel;
  };
  'ui.composer.read': {
    arguments: { paneId?: string };
    result: { text: string; revision: number };
  };
  'ui.composer.write': {
    arguments: { paneId?: string; text: string; revision: number };
    result: { text: string; revision: number };
  };
  'ui.navigate': {
    arguments: { target: 'home' | 'worktree' | 'code' | 'database' | 'new' };
    result: { target: string };
  };
  'ui.settings.show': {
    arguments: {
      section: import('./state/settings-view.svelte').SettingsSection;
      focus?: string | null;
    };
    result: { section: string };
  };
}

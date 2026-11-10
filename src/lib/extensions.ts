/**
 * The API 2 contribution contract, as data: what a manifest declares, and what a plugin is told.
 *
 * `catalog.json` is the same file package validation reads in Rust, so the surfaces and operations
 * the frontend offers cannot drift from the ones an install accepts.
 */
import catalog from '../../crates/wtm-plugins/contracts/catalog.json';
import type { AgentEvent, ChatEditSnapshot } from './ipc/types';
import type { Pane } from './state/sessions.svelte';
import type { Target } from './state/layout.svelte';
import type { SettingsSection } from './state/settings-view.svelte';
export { catalog };

/**
 * Where a contribution sits, as identity only: the workspace, and the pane, session, message or
 * tool call the containing feature is about. A plugin reads state through a snapshot operation;
 * it never receives it here. See `publicContext`.
 */
export type ExtensionContext = {
  projectId: string;
  worktreeId: string;
  paneId?: string;
  session?: string;
  messageId?: string;
  toolId?: string;
};

/** The keys `publicContext` lets out, and the only ones `{ "$context" = … }` can name. */
const CONTEXT_KEYS = [
  'projectId',
  'worktreeId',
  'paneId',
  'session',
  'messageId',
  'toolId',
] as const satisfies readonly (keyof ExtensionContext)[];

/**
 * The one way a context leaves for a plugin.
 *
 * An allow-list rather than a spread because the spread sent whatever a caller had put there: the
 * registry's internal `protected` flag, a message's full text, and once a transcript's private row
 * object with up to 2 MiB of tool output — over the runtime bridge's 1 MiB frame limit, so the
 * action failed rather than merely leaking.
 */
export function publicContext(context: ExtensionContext): ExtensionContext {
  const out: ExtensionContext = {
    projectId: context.projectId,
    worktreeId: context.worktreeId,
  };
  for (const key of CONTEXT_KEYS) {
    const value = context[key];
    if (typeof value === 'string') out[key] = value;
  }
  return out;
}

/** A context key by name, for `{ "$context" = … }` and `${context:…}`; undefined outside the list. */
export function contextValue(context: ExtensionContext, key: string): string | undefined {
  return (CONTEXT_KEYS as readonly string[]).includes(key)
    ? context[key as keyof ExtensionContext]
    : undefined;
}

/**
 * Every surface a component may mount, as a type, so `svelte-check` refuses a misspelt target the
 * way it refuses a misspelt class variant. Nothing else would: a target that names no surface just
 * renders nothing. `src-tauri/tests/extension_catalog.rs` holds this list equal to the catalog's
 * surfaces that take contributions, and `settings.plugins`, which takes none, is not in it.
 */
export type Surface =
  | 'chat.composer'
  | 'chat.composer.actions'
  | 'chat.markdown.code'
  | 'chat.markdown.link'
  | 'chat.markdown.paragraph'
  | 'chat.message'
  | 'chat.message.actions'
  | 'chat.tool'
  | 'chat.transcript'
  | 'code.actions'
  | 'database.actions'
  | 'native.activity'
  | 'native.session'
  | 'panel.actions'
  | 'panel.header'
  | 'session.new'
  | 'settings.actions'
  | 'settings.advanced'
  | 'settings.appearance'
  | 'settings.general'
  | 'settings.notifications'
  | 'settings.recipes'
  | 'workspace.actions'
  | 'workspace.toolbar';

export const PUBLIC_OPERATIONS: ReadonlySet<string> = new Set(
  catalog.operations.filter((o) => o.exposure === 'public').map((o) => o.id),
);

/** What each surface accepts. A target absent here, or with no kinds, accepts nothing. */
export const SURFACE_KINDS: ReadonlyMap<string, ReadonlySet<string>> = new Map(
  catalog.surfaces.map((s) => [s.id, new Set<string>(s.kinds)]),
);

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

/**
 * The `extension_choices` key for a style choice.
 *
 * Styles and replacements used to share one key per target, so a package shipping a style and a
 * replacement for the same target got neither by default and one select chose both.
 */
export function styleChoice(target: string): string {
  return `style:${target}`;
}

export type WorkbenchPanel = Pick<
  Pane,
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
    arguments: { paneId?: string; target: Target };
    result: WorkbenchPanel;
  };
  'ui.panels.resize': {
    arguments: { path: string; ratio: number };
    result: { path: string; ratio: number };
  };
  'ui.chat.snapshot': {
    arguments: { paneId?: string };
    result: ChatEditSnapshot & WorkbenchPanel & { events: AgentEvent[] };
  };
  'ui.chat.edit': {
    arguments: { paneId?: string; messageId?: string };
    result: { opened: boolean };
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
      section: SettingsSection;
      focus?: string | null;
    };
    result: { section: string };
  };
}

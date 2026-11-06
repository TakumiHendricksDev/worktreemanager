import type { ExtensionContributions, ExtensionPreferences } from './extensions';
/** The manifest contract is data; package code never imports private Svelte components. */
export interface PluginCommand {
  id: string;
  title: string;
  command: string[];
}
export interface PluginManifest {
  extensions: ExtensionContributions;
  schema: number;
  api: number;
  id: string;
  name: string;
  version: string;
  description: string;
  commands: PluginCommand[];
  panels: { id: string; title: string; file: string }[];
  session_backends: PluginCommand[];
  terminals: PluginCommand[];
  palettes: { id: string; title: string; hue: number; chroma: number; brand: string[] }[];
  platforms: string[];
  views: { id: string; title: string; file: string; slots: PluginSlot[] }[];
}
export const PLUGIN_SLOTS = {
  workbench: 'Entire workbench',
  titlebar: 'Window navigation',
  sidebar: 'Sidebar',
  worktree_bar: 'Worktree toolbar',
  home: 'Home',
  sessions: 'Session workbench',
  code: 'Code',
  database: 'Database',
  new_worktree: 'New worktree',
  settings_appearance: 'Appearance settings',
  settings_general: 'General settings',
  settings_notifications: 'Notification settings',
  settings_advanced: 'Advanced settings',
  settings_recipes: 'Recipe settings',
  settings_actions: 'Action settings',
} as const;
export type PluginSlot = keyof typeof PLUGIN_SLOTS;
export type PluginBinding =
  | { kind: 'builtin' }
  | { kind: 'hidden' }
  | { kind: 'plugin'; plugin: string; view: string };
export interface BuiltinPlugin {
  id: string;
  name: string;
  description: string;
  slots: PluginSlot[];
}
export interface PluginPackage {
  manifest: PluginManifest;
  source: string;
  kind: string;
  revision: string;
  digest: string;
  directory: string;
}
export interface PluginInstalled {
  current: PluginPackage;
  previous: PluginPackage | null;
  enabled: boolean;
}
export interface PluginsSnapshot {
  registry: {
    version: number;
    installed: Record<string, PluginInstalled>;
    disabled: string[];
    experience: string;
    safe_mode: boolean;
    bindings: Partial<Record<PluginSlot, PluginBinding>>;
    extensions: Record<string, ExtensionPreferences>;
    extension_choices: Record<string, string>;
    extension_order: string[];
  };
  problem: string | null;
  builtins: BuiltinPlugin[];
}
export interface PluginReview {
  token: string;
  package: PluginPackage;
  replacing: string | null;
}
export interface PluginRow {
  id: string;
  name: string;
  description: string;
  enabled: boolean;
  source: string;
  version: string;
  installed?: PluginInstalled;
}

export interface PluginDraft {
  validationError: string | null;
  id: string;
  provider: string;
  brief: string;
  folder: string;
  session: string | null;
  conversation: string | null;
  submitted: boolean;
  problem: string | null;
}
export interface PluginDraftView extends PluginDraft {
  status: 'starting' | 'building' | 'waiting' | 'error' | 'idle' | 'stopped';
  installed: string | null;
}

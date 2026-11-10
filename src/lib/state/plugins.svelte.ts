/**
 * The plugin registry as the app behaves now. Backend state is authoritative, including changes
 * from another window.
 *
 * `state.registry` is Rust's *effective* registry: in safe mode the overlay has already turned off
 * every user package and region choice the user has not touched this run, and a change made
 * during safe mode is both saved and shown. Reading `registry.safe_mode` here as well used to
 * second-guess that overlay, so a package turned on during safe mode looked off and its view never
 * mounted.
 */
import { theme } from './theme.svelte';
import { listen } from '@tauri-apps/api/event';
import { commands } from '../ipc/commands';
import { errorMessage } from '../ipc/types';
import {
  type PluginSlot,
  type PluginBinding,
  type PluginRow,
  type PluginsSnapshot,
} from '../plugins';

/** What `plugin_change` accepts. Anything else is "Unknown plugin operation" in Rust. */
type PluginAction =
  'install' | 'enable' | 'experience' | 'rollback' | 'uninstall' | 'recover';

/**
 * One operation a component offers, its in-flight state and its last failure.
 *
 * Each dialog and page owns one, so a failure is shown once, where it happened: a single shared
 * `error` was drawn by the page and again inside whichever dialog was open, and one left over from
 * a failed toggle reappeared in the next Add or Update dialog.
 */
export class PluginTask {
  busy = $state(false);
  error = $state<string | null>(null);

  /** Run `action` unless one is already running. True when it finished without throwing. */
  async run(action: () => Promise<unknown>): Promise<boolean> {
    if (this.busy) return false;
    this.busy = true;
    this.error = null;
    try {
      await action();
      return true;
    } catch (e) {
      this.error = errorMessage(e);
      return false;
    } finally {
      this.busy = false;
    }
  }

  clear(): void {
    this.error = null;
  }
}

class Plugins {
  state = $state<PluginsSnapshot>({
    registry: {
      version: 1,
      installed: {},
      disabled: [],
      experience: 'wtm.chat',
      safe_mode: false,
      bindings: {},
      extensions: {},
      extension_choices: {},
      extension_order: [],
    },
    safe_mode: false,
    problem: null,
    builtins: [],
  });
  ready = $state(false);
  /** Dismissed for the rest of this run; the notice itself is Rust's and lasts as long. */
  draftsNoticeDismissed = $state(false);
  enabled(id: string): boolean {
    if (id.startsWith('wtm.')) return !this.state.registry.disabled.includes(id);
    return !!this.state.registry.installed[id]?.enabled;
  }
  get rows(): PluginRow[] {
    return [
      ...this.state.builtins.map((p) => ({
        ...p,
        enabled: this.enabled(p.id),
        source: 'Built into wtm',
        version: 'Built-in',
      })),
      ...Object.values(this.state.registry.installed).map((p) => ({
        id: p.current.manifest.id,
        name: p.current.manifest.name,
        description: p.current.manifest.description,
        enabled: this.enabled(p.current.manifest.id),
        source: p.current.kind === 'git' ? 'Git repository' : 'Local folder',
        version: p.current.manifest.version,
        installed: p,
        slots: [],
      })),
    ];
  }
  async init() {
    const off = await listen<PluginsSnapshot>('plugins:changed', (event) => {
      this.state = event.payload;
      void theme.reload();
    });
    try {
      this.state = await commands.pluginsState();
    } catch {
      this.state.problem = 'Could not load plugin settings. Restart wtm to retry.';
    }
    this.ready = true;
    return off;
  }
  private async change(action: PluginAction, id = '', enabled = false) {
    this.state = await commands.pluginChange(action, id, enabled);
  }
  /** Install the reviewed snapshot a token names, and turn it on. */
  install(token: string) {
    return this.change('install', token);
  }
  setEnabled(id: string, enabled: boolean) {
    return this.change('enable', id, enabled);
  }
  setExperience(id: 'wtm.chat' | 'wtm.native-cli') {
    return this.change('experience', id);
  }
  rollback(id: string) {
    return this.change('rollback', id);
  }
  uninstall(id: string) {
    return this.change('uninstall', id);
  }
  recover() {
    return this.change('recover');
  }
  binding(slot: PluginSlot): PluginBinding {
    const binding = this.state.registry.bindings[slot];
    if (binding?.kind === 'hidden') return binding;
    if (
      binding?.kind === 'plugin' &&
      this.enabled(binding.plugin) &&
      this.state.registry.installed[binding.plugin]?.current.manifest.views.some(
        (v) => v.id === binding.view && v.slots.includes(slot),
      )
    )
      return binding;
    const builtin = this.state.builtins.find((p) => p.slots.includes(slot));
    return builtin && !this.enabled(builtin.id) ? { kind: 'hidden' } : { kind: 'builtin' };
  }
  available(slot: PluginSlot): boolean {
    return this.binding(slot).kind !== 'hidden';
  }
  async bind(slot: PluginSlot, binding: PluginBinding) {
    this.state = await commands.pluginBind(slot, binding);
  }
}
export const plugins = new Plugins();

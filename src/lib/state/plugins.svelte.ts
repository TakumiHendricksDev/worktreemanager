/** Backend state is authoritative, including changes from another window. */
import { theme } from './theme.svelte';
import { listen } from '@tauri-apps/api/event';
import { commands } from '../ipc/commands';
import {
  type PluginSlot,
  type PluginBinding,
  type PluginRow,
  type PluginsSnapshot,
} from '../plugins';
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
    problem: null,
    builtins: [],
  });
  ready = $state(false);
  enabled(id: string): boolean {
    if (id.startsWith('wtm.')) return !this.state.registry.disabled.includes(id);
    return !this.state.registry.safe_mode && !!this.state.registry.installed[id]?.enabled;
  }
  get rows(): PluginRow[] {
    return [
      ...this.state.builtins.map((p) => ({
        ...p,
        enabled: this.enabled(p.id),
        source: 'Built-in · WTM',
        version: 'Bundled',
      })),
      ...Object.values(this.state.registry.installed).map((p) => ({
        id: p.current.manifest.id,
        name: p.current.manifest.name,
        description: p.current.manifest.description,
        enabled: this.enabled(p.current.manifest.id),
        source: p.current.kind === 'git' ? 'Git repository' : 'Local development folder',
        version: p.current.manifest.version,
        installed: p,
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
      this.state.problem = 'Could not load plugin settings. Restart WTM to retry.';
    }
    this.ready = true;
    return off;
  }
  async change(action: string, id = '', enabled = false) {
    this.state = await commands.pluginChange(action, id, enabled);
  }
  binding(slot: PluginSlot): PluginBinding {
    if (this.state.registry.safe_mode) return { kind: 'builtin' };
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

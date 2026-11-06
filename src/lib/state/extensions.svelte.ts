/** Registrations are derived from reviewed snapshots; disabling never leaves imperative patches. */
import { setUiOperationInterceptor, withoutUiInterception } from '../extension-operations';
import { validateExtensionArguments } from '../extension-schema';
import { getContext } from 'svelte';
import { listen } from '@tauri-apps/api/event';
import {
  commands,
  dispatchExtensionOperation,
  setOperationMiddleware,
} from '../ipc/commands';
import { type InvokeArgs } from '@tauri-apps/api/core';
import { errorMessage } from '../ipc/types';
import { plugins } from './plugins.svelte';
import { workspace } from './workspace.svelte';
import { inPaneWindow } from '../window-role';
import {
  catalog,
  emptyExtensions,
  type Contribution,
  type ExtensionContext,
  type ExtensionContributions,
  type ExtensionAction,
} from '../extensions';

export const EXTENSION_PROTECTED = Symbol('wtm.extension.protected');
export const EXTENSION_CONTEXT = Symbol('wtm.extension.context');
export function extensionContext(): () => ExtensionContext {
  const protectedHost = getContext<boolean>(EXTENSION_PROTECTED) ?? false;
  const context =
    getContext<() => ExtensionContext>(EXTENSION_CONTEXT) ?? (() => extensions.context());
  return () => ({ ...context(), protected: protectedHost });
}
type Dispatcher = (
  operation: string,
  args: Record<string, unknown>,
  context: ExtensionContext,
) => Promise<unknown>;
class Extensions {
  problems = $state<Record<string, string>>({});
  ui: Dispatcher | null = null;
  context(): ExtensionContext {
    return {
      projectId: workspace.activeProjectId ?? '',
      worktreeId: workspace.selectedWorktreeId ?? '',
    };
  }
  active(plugin: string, context: ExtensionContext): boolean {
    if (context.protected) return false;
    const prefs = plugins.state.registry.extensions?.[plugin];
    return (
      plugins.enabled(plugin) &&
      (!prefs?.worktrees.length || prefs.worktrees.includes(context.worktreeId))
    );
  }
  contributions<K extends 'styles' | 'actions' | 'content' | 'settings' | 'hooks'>(
    kind: K,
    context: ExtensionContext,
  ): Contribution<ExtensionContributions[K][number]>[] {
    const out: Contribution<ExtensionContributions[K][number]>[] = [];
    for (const p of Object.values(plugins.state.registry.installed)) {
      const plugin = p.current.manifest.id;
      if (!this.active(plugin, context)) continue;
      const prefs = plugins.state.registry.extensions?.[plugin];
      for (const value of (p.current.manifest.extensions ?? emptyExtensions)[kind]) {
        if (!prefs?.disabled.includes(value.id))
          out.push({
            plugin,
            packageName: p.current.manifest.name,
            key: `${plugin}:${value.id}`,
            value,
          });
      }
    }
    const order = plugins.state.registry.extension_order ?? [];
    return out.sort((a, b) => {
      const ai = order.indexOf(a.key),
        bi = order.indexOf(b.key);
      return (
        (ai < 0 ? Number.MAX_SAFE_INTEGER : ai) - (bi < 0 ? Number.MAX_SAFE_INTEGER : bi) ||
        a.key.localeCompare(b.key)
      );
    });
  }
  settings(plugin: string, id: string, context: ExtensionContext): unknown {
    const p = plugins.state.registry.installed[
      plugin
    ]?.current.manifest.extensions?.settings.find((s) => s.id === id);
    const values = plugins.state.registry.extensions?.[plugin]?.values;
    return values?.[context.worktreeId]?.[id] ?? values?.['']?.[id] ?? p?.default;
  }
  resolve(value: unknown, plugin: string, context: ExtensionContext): unknown {
    if (value && typeof value === 'object' && !Array.isArray(value)) {
      const o = value as Record<string, unknown>;
      if (typeof o.$context === 'string') return context[o.$context];
      if (typeof o.$setting === 'string') return this.settings(plugin, o.$setting, context);
      return Object.fromEntries(
        Object.entries(o).map(([k, v]) => [k, this.resolve(v, plugin, context)]),
      );
    }
    if (Array.isArray(value)) return value.map((v) => this.resolve(v, plugin, context));
    return value;
  }
  text(value: string, plugin: string, context: ExtensionContext): string {
    return value.replace(/\$\{(setting|context):([a-zA-Z0-9_.-]+)\}/g, (_, kind, id) =>
      String(
        kind === 'setting'
          ? (this.settings(plugin, id, context) ?? '')
          : (context[id] ?? ''),
      ),
    );
  }
  style(target: string, context: ExtensionContext): string {
    const candidates = this.contributions('styles', context).filter(
      (s) => s.value.target === target,
    );
    const choice = plugins.state.registry.extension_choices?.[target];
    // A competing registration never wins merely because its package happened to load last.
    const selected =
      candidates.find((s) => s.key === choice) ??
      (!choice && candidates.length === 1 ? candidates[0] : null);
    return selected
      ? Object.entries(selected.value.properties)
          .map(([k, v]) => `${k}:${v}`)
          .join(';')
      : '';
  }
  winner<T extends { id: string }>(
    target: string,
    candidates: Contribution<T>[],
  ): Contribution<T> | undefined {
    const choice = plugins.state.registry.extension_choices?.[target];
    return (
      candidates.find((c) => c.key === choice) ??
      (!choice && candidates.length === 1 ? candidates[0] : undefined)
    );
  }
  async configure(plugin: string, change: unknown) {
    plugins.state = await commands.extensionConfigure(plugin, change);
  }
  async action(
    action: Contribution<ExtensionAction>,
    context: ExtensionContext,
  ): Promise<unknown> {
    if (!this.active(action.plugin, context))
      throw Error('Plugin is no longer active here');
    const args = this.resolve(action.value.arguments, action.plugin, context);
    try {
      if (action.value.operation.startsWith('plugin.'))
        return await commands.extensionCall(action.plugin, action.value.operation, {
          arguments: args,
          context,
        });
      return await commands.extensionHost(
        action.plugin,
        action.value.operation,
        args,
        context,
      );
    } catch (e) {
      this.problems[action.plugin] = errorMessage(e);
      throw e;
    }
  }
  async host(
    plugin: string,
    operation: string,
    args: unknown,
    context: ExtensionContext,
  ): Promise<unknown> {
    const manifest = plugins.state.registry.installed[plugin]?.current.manifest;
    if (!this.active(plugin, context) || !manifest?.extensions?.access.includes(operation))
      throw Error('Operation is outside this plugin’s reviewed access or workspace scope');
    if (!catalog.operations.some((o) => o.id === operation && o.exposure === 'public'))
      throw Error('Protected or unknown operation');
    validateExtensionArguments(operation, args);
    if (operation.startsWith('ui.')) {
      if (!this.ui) throw Error('Workbench is unavailable');
      return withoutUiInterception(() =>
        this.ui!(operation, args as Record<string, unknown>, context),
      );
    }
    return dispatchExtensionOperation(operation, args as InvokeArgs);
  }
  async hooks(
    operation: string,
    args: InvokeArgs,
    next: (args: InvokeArgs) => Promise<unknown>,
    provided?: ExtensionContext,
  ): Promise<unknown> {
    const original = args as Record<string, unknown>;
    const base = provided ?? this.context();
    const context = {
      ...base,
      worktreeId:
        typeof original.worktreeId === 'string' ? original.worktreeId : base.worktreeId,
      projectId:
        typeof original.projectId === 'string' ? original.projectId : base.projectId,
    };
    let arguments_ = args;
    const hooks = this.contributions('hooks', context).filter(
      (h) => h.value.operation === operation,
    );
    for (const h of hooks.filter((h) => h.value.phase === 'before')) {
      const result = (await commands.extensionCall(h.plugin, 'hook', {
        id: h.value.id,
        phase: 'before',
        operation,
        arguments: arguments_,
        context,
      })) as { deny?: string; arguments?: InvokeArgs } | null;
      if (result?.deny) throw Error(`${h.packageName}: ${result.deny}`);
      if (result?.arguments) {
        const before = arguments_ as Record<string, unknown>,
          after = result.arguments as Record<string, unknown>;
        for (const key of ['mode', 'approval', 'permissions'])
          if (JSON.stringify(before[key]) !== JSON.stringify(after[key]))
            throw Error('Hooks cannot change permission decisions');
        const oldOptions = before.options as Record<string, unknown> | undefined,
          newOptions = after.options as Record<string, unknown> | undefined;
        if (JSON.stringify(oldOptions?.mode) !== JSON.stringify(newOptions?.mode))
          throw Error('Hooks cannot change permission mode');
        if (
          typeof after.worktreeId === 'string' &&
          !this.active(h.plugin, { ...context, worktreeId: after.worktreeId })
        )
          throw Error('Hook target is outside its workspace scope');
        arguments_ = result.arguments;
        validateExtensionArguments(operation, arguments_);
        await commands.extensionValidate(h.plugin, operation, arguments_, context);
      }
    }
    const replacements = this.contributions('actions', context).filter(
      (a) => a.value.replaces === operation,
    );
    const replacement = this.winner(operation, replacements);
    const result = replacement
      ? await this.action(replacement, { ...context, arguments: arguments_ })
      : await next(arguments_);
    // Observers cannot turn a successful operation into an apparent failure and provoke a retry.
    for (const h of hooks.filter((h) => h.value.phase === 'after'))
      void commands
        .extensionCall(h.plugin, 'hook', {
          id: h.value.id,
          phase: 'after',
          operation,
          arguments: arguments_,
          result,
          context,
        })
        .catch((e) => {
          this.problems[h.plugin] = errorMessage(e);
        });
    if (!inPaneWindow)
      void commands
        .extensionEvent(
          'operation.completed',
          { operation, arguments: arguments_, result, context },
          context.worktreeId,
        )
        .catch(() => {});
    return result;
  }
  async init(): Promise<() => void> {
    const offPref = await listen('wtm:pref-changed', (e) => {
      if (!inPaneWindow)
        void commands.extensionEvent('settings.changed', e.payload, '').catch(() => {});
    });
    const offProblem = await listen<{ plugin: string; message: string }>(
      'extensions:problem',
      (e) => {
        this.problems[e.payload.plugin] = e.payload.message;
      },
    );
    setOperationMiddleware((operation, args, next) => this.hooks(operation, args, next));
    setUiOperationInterceptor((operation, args, context, apply) => {
      context = context.worktreeId ? context : this.context();
      const handles =
        this.contributions('hooks', context).some((h) => h.value.operation === operation) ||
        this.contributions('actions', context).some((a) => a.value.replaces === operation);
      if (!handles) return false;
      void this.hooks(
        operation,
        args,
        async (next) => {
          if (this.ui) {
            validateExtensionArguments(operation, next);
            return withoutUiInterception(() =>
              this.ui!(operation, next as Record<string, unknown>, context),
            );
          }
          apply(next as Record<string, unknown>);
          return null;
        },
        context,
      ).catch((e) => {
        this.problems.host = errorMessage(e);
      });
      return true;
    });
    let offHost = () => {};
    if (!inPaneWindow) {
      offHost = await listen<{
        id: string;
        plugin: string;
        operation: string;
        arguments: unknown;
        context: ExtensionContext;
      }>('extensions:host', (e) => {
        const r = e.payload;
        void this.host(r.plugin, r.operation, r.arguments, r.context).then(
          (result) => commands.extensionReply(r.id, result ?? null, null),
          (err) => commands.extensionReply(r.id, null, errorMessage(err)),
        );
      });
      this.problems = await commands.extensionStart();
    }
    return () => {
      offPref();
      offProblem();
      offHost();
      setOperationMiddleware(null);
      setUiOperationInterceptor(null);
    };
  }
}
export const extensions = new Extensions();
/** Semantic parts publish their own styles without exposing a selector into private markup. */
export function extensionPart(
  node: HTMLElement | SVGElement,
  params: { target: string | string[]; context?: () => ExtensionContext },
) {
  let current = $state(params);
  const original = node.getAttribute('style');
  const stop = $effect.root(() => {
    $effect(() => {
      const value = (Array.isArray(current.target) ? current.target : [current.target])
        .map((target) =>
          extensions.style(target, current.context?.() ?? extensions.context()),
        )
        .filter(Boolean)
        .join(';');
      if (value) node.setAttribute('style', [original, value].filter(Boolean).join(';'));
      else if (original !== null) node.setAttribute('style', original);
      else node.removeAttribute('style');
      node.dataset.wtmPart = Array.isArray(current.target)
        ? current.target.join(' ')
        : current.target;
    });
  });
  return {
    update(next: typeof params) {
      current = next;
    },
    destroy() {
      stop();
      if (original !== null) node.setAttribute('style', original);
      else node.removeAttribute('style');
      delete node.dataset.wtmPart;
    },
  };
}

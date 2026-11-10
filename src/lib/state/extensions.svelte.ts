/**
 * Which plugin contributions apply where, and the middleware that lets plugins take part in
 * operations.
 *
 * Registrations are derived from reviewed snapshots, so disabling a package never leaves an
 * imperative patch behind: the index below is rebuilt and whatever read it re-renders.
 *
 * # Why one index
 *
 * Parts sit on every Markdown paragraph and code span, and points on every transcript row. Asking
 * each of those "does anything target you?" by walking every installed manifest made the cost of a
 * streamed delta scale with the plugins installed. `#index` is rebuilt only when the plugin registry
 * changes, and a part or point whose target is absent from it does nothing further.
 *
 * # Why replacements wait for a choice
 *
 * Installing a package never chooses a replacement on the user's behalf (`docs/plugins.md`). A sole
 * candidate used to win by default, so enabling one package with `replaces = "<op>"` silently took
 * over a built-in operation. A replacement now applies only when `extension_choices` names it.
 * Styles only add to a part, so a sole style still applies; two compete, and then they wait too.
 *
 * # Why the middleware reads nothing reactively
 *
 * It runs inside whatever called a command, and that is often an `$effect` (Settings' doctor,
 * Home's refresh). Reading the registry synchronously there made the effect depend on it, so every
 * plugin setting toggle re-ran the doctor. Everything read before the first `await` is `untrack`ed.
 */
import { getContext, setContext, untrack } from 'svelte';
import type { Attachment } from 'svelte/attachments';
import { listen } from '@tauri-apps/api/event';
import { type InvokeArgs } from '@tauri-apps/api/core';
import { setUiOperationInterceptor } from '../extension-operations';
import { validateExtensionArguments, validateExtensionResult } from '../extension-schema';
import {
  commands,
  dispatchExtensionOperation,
  setOperationMiddleware,
} from '../ipc/commands';
import { errorMessage } from '../ipc/types';
import {
  contextValue,
  emptyExtensions,
  publicContext,
  PUBLIC_OPERATIONS,
  styleChoice,
  type Contribution,
  type ExtensionAction,
  type ExtensionContent,
  type ExtensionContext,
  type ExtensionHook,
  type ExtensionSetting,
  type ExtensionStyle,
  type Surface,
} from '../extensions';
import type { PluginsSnapshot } from '../plugins';
import { inPaneWindow } from '../window-role';
import { plugins } from './plugins.svelte';
import { workspace } from './workspace.svelte';

/**
 * Operations whose completion is never announced as `operation.completed`.
 *
 * Two reasons, and either is enough. Volume: `pty_*` is every keystroke and resize, and a replay is
 * a whole transcript or a mebibyte of terminal output. Contents: a file, a diff, a query's rows, an
 * agent's messages or an attachment's bytes would be handed to every subscribed package whether or
 * not it was ever meant to see them. A plugin that needs one of these can call it itself, under the
 * access it declared.
 */
const UNOBSERVED: readonly (string | RegExp)[] = [
  /^pty_/,
  /_replay$/,
  'code_read_file',
  'code_file_diff',
  'code_base_version',
  'code_search',
  'run_database_query',
  'database_table_page',
  'database_columns',
  'database_schemas',
  'database_relations',
  'agent_messages',
  'chat_edit_snapshot',
  'ui.chat.snapshot',
  'ui.composer.read',
  'prepare_agent_attachment',
  'stage_agent_attachment',
  'browser_snapshot_png',
];

function observable(operation: string): boolean {
  return !UNOBSERVED.some((rule) =>
    typeof rule === 'string' ? rule === operation : rule.test(operation),
  );
}

type Targeted = {
  styles: Contribution<ExtensionStyle>[];
  actions: Contribution<ExtensionAction>[];
  content: Contribution<ExtensionContent>[];
  settings: Contribution<ExtensionSetting>[];
};

type Index = {
  /** Styles, ordinary actions and content by `target`; settings by `section`. */
  surfaces: Map<string, Targeted>;
  hooks: Map<string, Contribution<ExtensionHook>[]>;
  /** Actions that stand in for an operation, by the operation. */
  replacements: Map<string, Contribution<ExtensionAction>[]>;
  /** By key, for a widget naming its own package's action or setting. */
  actions: Map<string, Contribution<ExtensionAction>>;
  settings: Map<string, Contribution<ExtensionSetting>>;
  /** Workspace scope per enabled package: null is everywhere. Absent is not enabled. */
  scopes: Map<string, ReadonlySet<string> | null>;
  /** Events some enabled package subscribes to. */
  events: Set<string>;
  /** Every enabled contribution, in the user's order. */
  ordered: Contribution<{ id: string; title: string }>[];
};

function build(snapshot: PluginsSnapshot): Index {
  const registry = snapshot.registry;
  const rank = new Map((registry.extension_order ?? []).map((key, at) => [key, at]));
  const index: Index = {
    surfaces: new Map(),
    hooks: new Map(),
    replacements: new Map(),
    actions: new Map(),
    settings: new Map(),
    scopes: new Map(),
    events: new Set(),
    ordered: [],
  };
  const surface = (target: string): Targeted => {
    let entry = index.surfaces.get(target);
    if (!entry) {
      entry = { styles: [], actions: [], content: [], settings: [] };
      index.surfaces.set(target, entry);
    }
    return entry;
  };
  const add = <T>(map: Map<string, T[]>, key: string, value: T) => {
    const list = map.get(key);
    if (list) list.push(value);
    else map.set(key, [value]);
  };
  for (const installed of Object.values(registry.installed)) {
    const manifest = installed.current.manifest;
    const plugin = manifest.id;
    if (!plugins.enabled(plugin)) continue;
    const preferences = registry.extensions?.[plugin];
    index.scopes.set(
      plugin,
      preferences?.worktrees.length ? new Set(preferences.worktrees) : null,
    );
    const declared = manifest.extensions ?? emptyExtensions;
    for (const name of declared.events) index.events.add(name);
    const wrap = <T extends { id: string; title: string }>(value: T) => {
      if (preferences?.disabled.includes(value.id)) return null;
      const contribution: Contribution<T> = {
        plugin,
        packageName: manifest.name,
        key: `${plugin}:${value.id}`,
        value,
      };
      index.ordered.push(contribution);
      return contribution;
    };
    for (const value of declared.styles) {
      const c = wrap(value);
      if (c) surface(value.target).styles.push(c);
    }
    for (const value of declared.actions) {
      const c = wrap(value);
      if (!c) continue;
      index.actions.set(c.key, c);
      if (value.replaces) add(index.replacements, value.replaces, c);
      else surface(value.target).actions.push(c);
    }
    for (const value of declared.content) {
      const c = wrap(value);
      if (c) surface(value.target).content.push(c);
    }
    for (const value of declared.settings) {
      const c = wrap(value);
      if (!c) continue;
      index.settings.set(c.key, c);
      surface(value.section).settings.push(c);
    }
    for (const value of declared.hooks) {
      const c = wrap(value);
      if (c) add(index.hooks, value.operation, c);
    }
  }
  // A competing registration never wins merely because its package happened to load last.
  const byOrder = (a: { key: string }, b: { key: string }) =>
    (rank.get(a.key) ?? Number.MAX_SAFE_INTEGER) -
      (rank.get(b.key) ?? Number.MAX_SAFE_INTEGER) || a.key.localeCompare(b.key);
  for (const entry of index.surfaces.values()) {
    entry.styles.sort(byOrder);
    entry.actions.sort(byOrder);
    entry.content.sort(byOrder);
    entry.settings.sort(byOrder);
  }
  for (const list of index.hooks.values()) list.sort(byOrder);
  for (const list of index.replacements.values()) list.sort(byOrder);
  index.ordered.sort(byOrder);
  return index;
}

/** One `extension_choices` entry the user can make. */
export type Choice = {
  key: string;
  candidates: Contribution<{ id: string; title: string }>[];
};

type Dispatcher = (
  operation: string,
  args: Record<string, unknown>,
  context: ExtensionContext,
) => Promise<unknown>;

class Extensions {
  problems = $state<Record<string, string>>({});
  /** The frontend-owned `ui.*` operations. Set by `initExtensionWorkbench` in the main window. */
  ui: Dispatcher | null = null;
  readonly #index = $derived(build(plugins.state));

  /** The workspace on screen, for features that are not about a narrower one. */
  context(): ExtensionContext {
    return {
      projectId: workspace.activeProjectId ?? '',
      worktreeId: workspace.selectedWorktreeId ?? '',
    };
  }

  /** Whether any enabled package aims anything at a surface, wherever it is scoped. A lookup. */
  targets(target: Surface): boolean {
    return this.#index.surfaces.has(target);
  }

  /** Whether any enabled package styles a part. */
  styled(target: Surface): boolean {
    return !!this.#index.surfaces.get(target)?.styles.length;
  }

  subscribed(event: string): boolean {
    return this.#index.events.has(event);
  }

  active(plugin: string, context: ExtensionContext): boolean {
    const scope = this.#index.scopes.get(plugin);
    return scope !== undefined && (scope === null || scope.has(context.worktreeId));
  }

  content(
    target: string,
    position: ExtensionContent['position'],
    context: ExtensionContext,
  ): Contribution<ExtensionContent>[] {
    return (this.#index.surfaces.get(target)?.content ?? []).filter(
      (c) => c.value.position === position && this.active(c.plugin, context),
    );
  }

  actions(target: string, context: ExtensionContext): Contribution<ExtensionAction>[] {
    return (this.#index.surfaces.get(target)?.actions ?? []).filter((c) =>
      this.active(c.plugin, context),
    );
  }

  settings(section: string, context: ExtensionContext): Contribution<ExtensionSetting>[] {
    return (this.#index.surfaces.get(section)?.settings ?? []).filter((c) =>
      this.active(c.plugin, context),
    );
  }

  /** One of a package's own declared actions or settings, by id, where it is active. */
  action(plugin: string, id: string, context: ExtensionContext) {
    const c = this.#index.actions.get(`${plugin}:${id}`);
    return c && this.active(plugin, context) ? c : undefined;
  }
  setting(plugin: string, id: string, context: ExtensionContext) {
    const c = this.#index.settings.get(`${plugin}:${id}`);
    return c && this.active(plugin, context) ? c : undefined;
  }

  /** Every enabled contribution in the user's order, wherever it is scoped. */
  get ordered(): readonly Contribution<{ id: string; title: string }>[] {
    return this.#index.ordered;
  }

  /**
   * Everything that needs a choice, by its `extension_choices` key: each styled part, and each
   * operation something can replace. Wherever the candidates are scoped, since a choice is global.
   */
  get choices(): Choice[] {
    const out: Choice[] = [];
    for (const [target, entry] of this.#index.surfaces)
      if (entry.styles.length)
        out.push({ key: styleChoice(target), candidates: entry.styles });
    for (const [operation, candidates] of this.#index.replacements)
      out.push({ key: operation, candidates });
    return out;
  }

  /** The style a part wears here: the chosen one, or the only one. */
  style(target: string, context: ExtensionContext): Record<string, string> | null {
    const candidates = (this.#index.surfaces.get(target)?.styles ?? []).filter((c) =>
      this.active(c.plugin, context),
    );
    const choice = plugins.state.registry.extension_choices?.[styleChoice(target)];
    const selected =
      candidates.find((c) => c.key === choice) ??
      (!choice && candidates.length === 1 ? candidates[0] : undefined);
    return selected?.value.properties ?? null;
  }

  /** The replacement the user chose for an operation, if it is still a candidate here. */
  #replacement(
    operation: string,
    context: ExtensionContext | null,
    except?: string,
  ): Contribution<ExtensionAction> | undefined {
    const choice = plugins.state.registry.extension_choices?.[operation];
    if (!choice) return undefined;
    return this.#index.replacements
      .get(operation)
      ?.find(
        (c) =>
          c.key === choice &&
          c.plugin !== except &&
          (context === null || this.active(c.plugin, context)),
      );
  }

  /** Whether a hook or a chosen replacement takes part in an operation. Untracked. */
  #intercepted(operation: string, except?: string): boolean {
    return untrack(
      () =>
        (this.#index.hooks.get(operation) ?? []).some((h) => h.plugin !== except) ||
        this.#replacement(operation, null, except) !== undefined,
    );
  }

  /** Whether calling an operation needs the middleware at all. Untracked. */
  intercepts(operation: string, except?: string): boolean {
    return (
      this.#intercepted(operation, except) ||
      untrack(() => this.subscribed('operation.completed') && observable(operation))
    );
  }

  settingValue(plugin: string, id: string, context: ExtensionContext): unknown {
    const registry = plugins.state.registry;
    const declared = registry.installed[plugin]?.current.manifest.extensions?.settings.find(
      (s) => s.id === id,
    );
    const values = registry.extensions?.[plugin]?.values;
    return values?.[context.worktreeId]?.[id] ?? values?.['']?.[id] ?? declared?.default;
  }

  /**
   * An action's `arguments`, with `{ "$context" = … }` and `{ "$setting" = … }` filled in.
   *
   * A replacement may also name `{ "$context" = "arguments" }` for the arguments of the operation
   * it stands in for, which no other context carries.
   */
  resolve(
    value: unknown,
    plugin: string,
    context: ExtensionContext,
    intercepted?: InvokeArgs,
  ): unknown {
    if (Array.isArray(value))
      return value.map((v) => this.resolve(v, plugin, context, intercepted));
    if (value && typeof value === 'object') {
      const o = value as Record<string, unknown>;
      if (typeof o.$context === 'string')
        return o.$context === 'arguments' && intercepted !== undefined
          ? intercepted
          : contextValue(context, o.$context);
      if (typeof o.$setting === 'string')
        return this.settingValue(plugin, o.$setting, context);
      return Object.fromEntries(
        Object.entries(o).map(([k, v]) => [
          k,
          this.resolve(v, plugin, context, intercepted),
        ]),
      );
    }
    return value;
  }

  text(value: string, plugin: string, context: ExtensionContext): string {
    return value.replace(/\$\{(setting|context):([a-zA-Z0-9_.-]+)\}/g, (_, kind, id) =>
      String(
        kind === 'setting'
          ? (this.settingValue(plugin, id, context) ?? '')
          : (contextValue(context, id) ?? ''),
      ),
    );
  }

  async configure(plugin: string, change: unknown) {
    plugins.state = await commands.extensionConfigure(plugin, change);
  }

  /** Run a contribution's action. `intercepted` is set when it stands in for an operation. */
  async perform(
    action: Contribution<ExtensionAction>,
    context: ExtensionContext,
    intercepted?: InvokeArgs,
  ): Promise<unknown> {
    const args = untrack(() => {
      if (!this.active(action.plugin, context))
        throw Error('Plugin is no longer active here');
      return this.resolve(action.value.arguments, action.plugin, context, intercepted);
    });
    const outgoing = publicContext(context);
    try {
      if (action.value.operation.startsWith('plugin.'))
        return await commands.extensionCall(action.plugin, action.value.operation, {
          arguments: args,
          context: outgoing,
        });
      return await commands.extensionHost(
        action.plugin,
        action.value.operation,
        args,
        outgoing,
      );
    } catch (e) {
      this.problems[action.plugin] = errorMessage(e);
      throw e;
    }
  }

  /** A plugin's own request to the host, as relayed by Rust with the context it bound. */
  async host(
    plugin: string,
    operation: string,
    args: unknown,
    context: ExtensionContext,
  ): Promise<unknown> {
    const manifest = plugins.state.registry.installed[plugin]?.current.manifest;
    if (!this.active(plugin, context) || !manifest?.extensions?.access.includes(operation))
      throw Error('Operation is outside this plugin’s reviewed access or workspace scope');
    if (!PUBLIC_OPERATIONS.has(operation)) throw Error('Protected or unknown operation');
    validateExtensionArguments(operation, args);
    if (!operation.startsWith('ui.'))
      return dispatchExtensionOperation(operation, args as InvokeArgs);
    const ui = this.ui;
    if (!ui) throw Error('Workbench is unavailable');
    const apply = (next: InvokeArgs) =>
      ui(operation, next as Record<string, unknown>, context);
    // Through other packages' hooks, as a host-initiated call would be, but never the requester's
    // own: its runtime may be blocked waiting on this very reply, and its own replacement would
    // only call this operation again.
    return this.intercepts(operation, plugin)
      ? this.hooks(operation, args as InvokeArgs, apply, context, plugin)
      : apply(args as InvokeArgs);
  }

  /**
   * Hooks, the chosen replacement, then `operation.completed`, around one operation.
   *
   * `requester` is the package that asked for the operation, if one did. Its own hooks and
   * replacement are skipped, which is also what stops a replacement that calls the operation it
   * replaces from replacing itself forever.
   */
  async hooks(
    operation: string,
    args: InvokeArgs,
    next: (args: InvokeArgs) => Promise<unknown>,
    provided?: ExtensionContext,
    requester?: string,
  ): Promise<unknown> {
    const { context, before, after, replacement } = untrack(() => {
      const original = args as Record<string, unknown>;
      const base = provided ?? this.context();
      const context: ExtensionContext = {
        ...base,
        worktreeId:
          typeof original.worktreeId === 'string' ? original.worktreeId : base.worktreeId,
        projectId:
          typeof original.projectId === 'string' ? original.projectId : base.projectId,
      };
      const hooks = (this.#index.hooks.get(operation) ?? []).filter(
        (h) => h.plugin !== requester && this.active(h.plugin, context),
      );
      return {
        context,
        before: hooks.filter((h) => h.value.phase === 'before'),
        after: hooks.filter((h) => h.value.phase === 'after'),
        replacement: this.#replacement(operation, context, requester),
      };
    });
    const outgoing = publicContext(context);
    let arguments_ = args;
    for (const h of before) {
      const result = (await commands.extensionCall(h.plugin, 'hook', {
        id: h.value.id,
        phase: 'before',
        operation,
        arguments: arguments_,
        context: outgoing,
      })) as { deny?: string; arguments?: InvokeArgs } | null;
      if (result?.deny) throw Error(`${h.packageName}: ${result.deny}`);
      if (result?.arguments) {
        const previous = arguments_ as Record<string, unknown>,
          changed = result.arguments as Record<string, unknown>;
        for (const key of ['mode', 'approval', 'permissions'])
          if (JSON.stringify(previous[key]) !== JSON.stringify(changed[key]))
            throw Error('Hooks cannot change permission decisions');
        const oldOptions = previous.options as Record<string, unknown> | undefined,
          newOptions = changed.options as Record<string, unknown> | undefined;
        if (JSON.stringify(oldOptions?.mode) !== JSON.stringify(newOptions?.mode))
          throw Error('Hooks cannot change permission mode');
        if (
          typeof changed.worktreeId === 'string' &&
          !this.active(h.plugin, { ...context, worktreeId: changed.worktreeId })
        )
          throw Error('Hook target is outside its workspace scope');
        arguments_ = result.arguments;
        validateExtensionArguments(operation, arguments_);
        await commands.extensionValidate(h.plugin, operation, arguments_, outgoing);
      }
    }
    let result: unknown;
    if (replacement) {
      result = (await this.perform(replacement, context, arguments_)) ?? null;
      validateExtensionResult(operation, result);
    } else result = await next(arguments_);
    // Observers cannot turn a successful operation into an apparent failure and provoke a retry.
    for (const h of after)
      void commands
        .extensionCall(h.plugin, 'hook', {
          id: h.value.id,
          phase: 'after',
          operation,
          arguments: arguments_,
          result,
          context: outgoing,
        })
        .catch((e) => {
          this.problems[h.plugin] = errorMessage(e);
        });
    if (!inPaneWindow && this.subscribed('operation.completed') && observable(operation))
      void commands
        .extensionEvent(
          'operation.completed',
          { operation, arguments: arguments_, result, context: outgoing },
          context.worktreeId,
        )
        .catch(() => {});
    return result;
  }

  /** Install the middleware and listeners. Never rejects: plugins are not worth a failed boot. */
  async init(): Promise<() => void> {
    const offs: (() => void)[] = [];
    setOperationMiddleware({
      intercepts: (operation) => this.intercepts(operation),
      run: (operation, args, next) => this.hooks(operation, args, next),
    });
    setUiOperationInterceptor({
      intercepts: (operation) => this.#intercepted(operation),
      context: (partial) => (partial.worktreeId ? partial : untrack(() => this.context())),
      run: async (operation, args, context) => {
        try {
          await this.hooks(
            operation,
            args,
            async (next) => {
              const ui = this.ui;
              if (!ui) throw Error('Workbench is unavailable');
              validateExtensionArguments(operation, next);
              return ui(operation, next as Record<string, unknown>, context);
            },
            context,
          );
        } catch (e) {
          this.problems.host = errorMessage(e);
        }
      },
    });
    offs.push(() => {
      setOperationMiddleware(null);
      setUiOperationInterceptor(null);
    });
    try {
      offs.push(
        await listen('wtm:pref-changed', (e) => {
          if (!inPaneWindow && this.subscribed('settings.changed'))
            void commands.extensionEvent('settings.changed', e.payload, '').catch(() => {});
        }),
        await listen<{ plugin: string; message: string }>('extensions:problem', (e) => {
          this.problems[e.payload.plugin] = e.payload.message;
        }),
      );
      if (!inPaneWindow) {
        offs.push(
          await listen<{
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
          }),
        );
        this.problems = await commands.extensionStart();
      }
    } catch (e) {
      this.problems.host = errorMessage(e);
    }
    return () => {
      for (const off of offs.reverse()) off();
    };
  }
}

export const extensions = new Extensions();

const EXTENSION_PROTECTED = Symbol('wtm.extension.protected');
const EXTENSION_CONTEXT = Symbol('wtm.extension.context');

/**
 * The context of the feature a component is mounted in, or null under a protected host.
 *
 * Null rather than a flag on the context, because the flag used to ride along in every context
 * sent to a plugin. Without a provider above it, a component is about the workspace on screen.
 */
export function extensionContext(): (() => ExtensionContext) | null {
  if (getContext<boolean | undefined>(EXTENSION_PROTECTED)) return null;
  return (
    getContext<(() => ExtensionContext) | undefined>(EXTENSION_CONTEXT) ??
    (() => extensions.context())
  );
}

/** Name the pane everything below is about. Under a protected host it stays protected. */
export function provideExtensionContext(
  own: () => ExtensionContext,
): (() => ExtensionContext) | null {
  if (getContext<boolean | undefined>(EXTENSION_PROTECTED)) return null;
  setContext(EXTENSION_CONTEXT, own);
  return own;
}

/** A pane's identity, as a context, for a component that draws one. */
export function paneContext(pane: {
  id: string;
  projectId: string;
  worktreeId: string;
  session: string | null;
}): ExtensionContext {
  return {
    projectId: pane.projectId,
    worktreeId: pane.worktreeId,
    paneId: pane.id,
    ...(pane.session ? { session: pane.session } : {}),
  };
}

/** Keep every contribution out of what this component renders. */
export function protectFromExtensions(): void {
  setContext(EXTENSION_PROTECTED, true);
}

/**
 * A semantic part's style, as an attachment: `{@attach extensionPart('chat.message', context)}`.
 *
 * Undefined when nothing styles the target or the part is under a protected host, and Svelte
 * creates no effect for an undefined attachment, so an unstyled paragraph costs one map lookup.
 *
 * It sets and removes only the properties the chosen style declares. It used to own the whole
 * `style` attribute, snapshotting it at mount and rewriting it later, which threw away anything the
 * host had set since — `style:` directives included.
 */
export function extensionPart(
  target: Surface,
  context: (() => ExtensionContext) | null,
): Attachment<HTMLElement | SVGElement> | undefined {
  if (!context || !extensions.styled(target)) return undefined;
  return (node) => {
    const properties = extensions.style(target, context());
    if (!properties) return;
    const restore: { name: string; value: string; priority: string; ours: string }[] = [];
    for (const [name, value] of Object.entries(properties)) {
      const previous = node.style.getPropertyValue(name);
      const priority = node.style.getPropertyPriority(name);
      node.style.setProperty(name, value);
      restore.push({
        name,
        value: previous,
        priority,
        ours: node.style.getPropertyValue(name),
      });
    }
    return () => {
      for (const { name, value, priority, ours } of restore) {
        // The host may have set the property since; then it is the host's again, not ours.
        if (node.style.getPropertyValue(name) !== ours) continue;
        if (value) node.style.setProperty(name, value, priority);
        else node.style.removeProperty(name);
      }
    };
  };
}

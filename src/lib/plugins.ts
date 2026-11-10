import {
  catalog,
  type ExtensionContributions,
  type ExtensionPreferences,
} from './extensions';
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
/**
 * Regions whose loss leaves no visible way back, so hiding one asks first. Everything else that
 * can be hidden leaves the window usable, and Settings stays reachable with ⌘, either way.
 */
export const HIDING_ASKS: ReadonlySet<PluginSlot> = new Set([
  'workbench',
  'titlebar',
  'sessions',
]);
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
/** Where a package came from. Rust refuses any other value, so the type does too. */
export type PluginSourceKind = 'local' | 'git';
export interface PluginPackage {
  manifest: PluginManifest;
  source: string;
  kind: PluginSourceKind;
  revision: string;
  /** The tag, branch or commit as typed, which Review update offers again; null for a folder. */
  requested: string | null;
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
  /** WTM started with `--safe-mode`: user plugins are off for this run, not in the saved file. */
  safe_mode: boolean;
  problem: string | null;
  builtins: BuiltinPlugin[];
}
export interface PluginReview {
  token: string;
  package: PluginPackage;
  /** The installed digest this would replace; equal to `package.digest` when nothing changed. */
  replacing: string | null;
  /** The host's rule for "acts as native code": executables, a runtime, or any declared access. */
  executable: boolean;
}
/** Where the Add / Review update dialog opens: choosing a source, or straight at a review. */
export type PluginReviewStart =
  | { step: 'source'; kind: PluginSourceKind; update: PluginInstalled | null }
  | { step: 'review'; review: PluginReview; draft: PluginDraftView | null };
export interface PluginRow {
  id: string;
  name: string;
  description: string;
  enabled: boolean;
  source: string;
  version: string;
  installed?: PluginInstalled;
  /** The regions a built-in fills; empty for a package. */
  slots: readonly PluginSlot[];
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

/**
 * Whether a package acts as native code, for a package already installed.
 *
 * The same rule as `Manifest::executable` in `wtm-plugins`, which a review reports as
 * `PluginReview.executable`; keep the two in step. Declared host access counts without any
 * program, because public operations still read files and transcripts and change worktrees with
 * the user's authority. That is why a package with access is never described as isolated.
 */
export function executes(manifest: PluginManifest): boolean {
  const x = manifest.extensions;
  return (
    x.runtime.length > 0 ||
    x.access.length > 0 ||
    manifest.commands.length > 0 ||
    manifest.session_backends.length > 0 ||
    manifest.terminals.length > 0
  );
}

/**
 * An argv as one line a reader can split back into its arguments.
 *
 * `join(' ')` could not tell `["run", "a b"]` from `["run", "a", "b"]`, and `JSON.stringify`
 * could but read as data rather than as a command. Quoting the way a shell would is the notation a
 * developer already parses; it describes the arguments and implies nothing about a shell, since
 * none runs.
 */
export function argvText(argv: readonly string[]): string {
  return argv
    .map((part) =>
      /^[\w@%+=:,./-]+$/.test(part) ? part : `'${part.replace(/'/g, `'\\''`)}'`,
    )
    .join(' ');
}

/** A Git commit shortened for a sentence; anything else is left as it was given. */
export function shortRevision(revision: string): string {
  return /^[0-9a-f]{40}$/.test(revision) ? revision.slice(0, 12) : revision;
}

/** Version and revision together, as an update describes the two ends of itself. */
export function versionLabel(p: PluginPackage): string {
  return p.kind === 'git'
    ? `${p.manifest.version} (${shortRevision(p.revision)})`
    : p.manifest.version;
}

/** The parts of a package that live outside wtm's own screens. */
export type PackagePartKind =
  'terminal' | 'view' | 'palette' | 'command' | 'panel' | 'backend';
export const PACKAGE_PART_LABELS: Record<PackagePartKind, string> = {
  terminal: 'Terminal profile',
  view: 'Replacement view',
  palette: 'Palette',
  command: 'Command',
  panel: 'Isolated panel',
  backend: 'Session backend',
};
export interface PackagePart {
  kind: PackagePartKind;
  id: string;
  title: string;
  /** What a reviewer needs besides the title: the regions a view fills, a panel's file. */
  detail: string | null;
  /** What runs, for the kinds that run something. */
  argv: string[] | null;
}

/** Every such part, in the order the details have always listed them. */
export function packageParts(m: PluginManifest): PackagePart[] {
  const run = (kind: PackagePartKind, c: PluginCommand): PackagePart => ({
    kind,
    id: c.id,
    title: c.title,
    detail: null,
    argv: c.command,
  });
  return [
    ...m.terminals.map((c) => run('terminal', c)),
    ...m.views.map((v) => ({
      kind: 'view' as const,
      id: v.id,
      title: v.title,
      detail: v.slots.map((s) => PLUGIN_SLOTS[s]).join(', '),
      argv: null,
    })),
    ...m.palettes.map((p) => ({
      kind: 'palette' as const,
      id: p.id,
      title: p.title,
      detail: null,
      argv: null,
    })),
    ...m.commands.map((c) => run('command', c)),
    ...m.panels.map((p) => ({
      kind: 'panel' as const,
      id: p.id,
      title: p.title,
      detail: p.file,
      argv: null,
    })),
    ...m.session_backends.map((c) => run('backend', c)),
  ];
}

/** A surface's name as the catalog gives it, or its id when the catalog has no such surface. */
export function surfaceTitle(id: string): string {
  return catalog.surfaces.find((s) => s.id === id)?.title ?? id;
}

/** Why the catalog says an operation exists, for a review listing what a package may call. */
export function operationReason(id: string): string {
  return (
    catalog.operations.find((o) => o.id === id)?.reason ??
    'Not in this version of the public contract.'
  );
}

export function eventReason(id: string): string {
  return (
    catalog.events.find((e) => e.id === id)?.reason ??
    'Not in this version of the public contract.'
  );
}

/** One in-feature contribution as Settings lists it. */
export interface ContributionRow {
  id: string;
  key: string;
  title: string;
  /** What kind of addition, in words: "Action", "Content above", "Runs before". */
  kind: string;
  /** Where it applies, in words when the catalog names the place. */
  place: string;
  /** Whether `place` is an operation id, which has no name of its own and is shown as code. */
  operation: boolean;
  /**
   * What it competes with: contributions in one group are ordered relative to each other and to
   * nothing else, because that is the only order the registry reads (`build` in
   * `extensions.svelte.ts` sorts each target's list, each operation's hooks and each operation's
   * replacements separately).
   */
  group: string;
}

/** A package's declared in-feature contributions, in manifest order. */
export function contributionRows(
  plugin: string,
  declared: ExtensionContributions,
): ContributionRow[] {
  const key = (id: string) => `${plugin}:${id}`;
  const position = { before: 'Content above', after: 'Content below', replace: 'Content' };
  return [
    ...declared.styles.map((v) => ({
      id: v.id,
      key: key(v.id),
      title: v.title,
      kind: 'Style',
      place: surfaceTitle(v.target),
      operation: false,
      group: `style:${v.target}`,
    })),
    ...declared.actions.map((v) =>
      v.replaces
        ? {
            id: v.id,
            key: key(v.id),
            title: v.title,
            kind: 'Replaces',
            place: v.replaces,
            operation: true,
            group: `replace:${v.replaces}`,
          }
        : {
            id: v.id,
            key: key(v.id),
            title: v.title,
            kind: 'Action',
            place: surfaceTitle(v.target),
            operation: false,
            group: `action:${v.target}`,
          },
    ),
    ...declared.content.map((v) => ({
      id: v.id,
      key: key(v.id),
      title: v.title,
      kind: position[v.position],
      place: surfaceTitle(v.target),
      operation: false,
      group: `content:${v.target}:${v.position}`,
    })),
    ...declared.settings.map((v) => ({
      id: v.id,
      key: key(v.id),
      title: v.title,
      kind: 'Setting',
      place: surfaceTitle(v.section),
      operation: false,
      group: `setting:${v.section}`,
    })),
    ...declared.hooks.map((v) => ({
      id: v.id,
      key: key(v.id),
      title: v.title,
      kind: v.phase === 'before' ? 'Runs before' : 'Runs after',
      place: v.operation,
      operation: true,
      group: `hook:${v.operation}:${v.phase}`,
    })),
  ];
}

/**
 * The registry's order as a comparator: saved keys by position, then everything unsaved by key.
 * The same rule as `byOrder` in `extensions.svelte.ts`, so what Settings shows is what applies.
 */
export function orderBy(saved: readonly string[]) {
  const rank = new Map(saved.map((k, at) => [k, at]));
  return (a: string, b: string) =>
    (rank.get(a) ?? Number.MAX_SAFE_INTEGER) - (rank.get(b) ?? Number.MAX_SAFE_INTEGER) ||
    a.localeCompare(b);
}

/**
 * The saved order with one group rearranged and nothing else moved.
 *
 * `{ kind: 'order' }` replaces the whole saved list, so a move has to carry every other key with
 * it in place. Group members that were never saved rank after every saved key; they are appended in
 * that order first, which changes nothing about how they compare, and then the group's new order
 * is written back into exactly the slots its members occupy.
 */
export function mergeOrder(saved: readonly string[], group: readonly string[]): string[] {
  const members = new Set(group);
  const next = [...saved, ...group.filter((k) => !saved.includes(k))];
  const queue = [...group];
  return next.map((k) => (members.has(k) ? (queue.shift() ?? k) : k));
}

/**
 * Settings › Recipes: a project's review recipes and the roles they run, as the editor holds them.
 *
 * The types mirror `wtm_core::flow::recipe` and `src-tauri/src/recipes.rs` as serde writes them.
 * A role keeps the config's own spelling (`timeout_ms`, `allow_credit_billing`) rather than a
 * camelCase copy, because the editor sends the whole role back to be written into the config, and a
 * validation error names its key in that spelling (`role.conventions.retries`): one spelling end to
 * end is what lets an error land on the field it is about without a table translating between two.
 *
 * Plain, beside `knowledge.ts`, for the reason `work.ts` gives: with no JS test runner, logic
 * reachable only from inside a component cannot be reasoned about in isolation. Every edit here is
 * a pure function from one draft to the next, which is also what keeps Discard honest.
 */
import type { RoleProvider, SkillProblem } from './ipc/types';

export type WorkflowHome = 'defaults' | 'project' | 'local';
export type RoleInput =
  'issue' | 'plan' | 'design' | 'diff' | 'rules' | 'instructions' | 'findings' | 'answers';
export type OutputKind = 'findings' | 'plan' | 'handoff';
export type RoleProfile = 'reviewer' | 'writer';
export type Workspace = 'snapshot' | 'worktree';

/** `"ask"`, one role, or up to three tried in order. */
export type Fallback = string | string[];

export interface KnowledgeScope {
  areas?: string[];
  max_rules: number | null;
}

/** A role. Keys serde leaves out at their default may be absent; absent means the default. */
export interface RoleSpec {
  objective: string;
  instructions: string | null;
  provider: RoleProvider;
  model: string | null;
  effort: string | null;
  skills?: string[];
  mcp?: string[];
  plugins?: string[];
  knowledge?: KnowledgeScope;
  paths?: string[];
  checks?: string[];
  inputs: RoleInput[];
  output: OutputKind;
  profile: RoleProfile;
  workspace: Workspace | null;
  timeout_ms: number | null;
  retries: number;
  fallback: Fallback;
  allow_credit_billing?: boolean;
  kind?: 'static' | 'runtime';
  service?: string | null;
  driver?: 'playwright' | 'browser' | null;
}

export interface StageSpec {
  roles: string[];
}

export interface RecipeSpec {
  label: string | null;
  stage: StageSpec[];
  concurrency: number | null;
}

export interface ImplementSpec {
  writer: string;
  recipe: string;
  checks?: string[] | null;
  fix_rounds: number;
  fix_attempts: number;
}

/** What Settings edits and sends back whole. */
export interface WorkflowDraft {
  role: Record<string, RoleSpec>;
  recipe: Record<string, RecipeSpec>;
}

export interface WorkflowFile {
  /** Where an edit is written; null when nothing declares a workflow yet and Save asks. */
  home: WorkflowHome | null;
  declaring: WorkflowHome[];
  roleHomes: Record<string, WorkflowHome[]>;
  recipeHomes: Record<string, WorkflowHome[]>;
  userPath: string;
  localPath: string;
  rootKey: string;
  /** Sent back with a save, which is refused when either file changed since. */
  revision: string;
}

export interface PlaceView {
  home: WorkflowHome;
  header: string;
  path: string;
}

export interface RecipeSettingsView {
  file: WorkflowFile;
  places: PlaceView[];
  workflow: {
    role: Record<string, RoleSpec>;
    recipe: Record<string, RecipeSpec>;
    planner: string | null;
    designer: string | null;
    implement: ImplementSpec | null;
    checks: { id: string; label: string }[];
  };
}

export interface ProviderSkills {
  provider: RoleProvider;
  /** Null when the provider could not be asked; `problem` says why. */
  listed: string[] | null;
  problem: string | null;
}

export interface SkillCheckView {
  /** The roles it checked: a mark on any other role would be a guess. */
  roles: string[];
  providers: ProviderSkills[];
  problems: SkillProblem[];
  /**
   * Not Rust's: set when the marks come from a run's report rather than a check. A run stops at the
   * first skill its provider lacks, so that one skill is known missing and the rest are not known.
   */
  reported?: { role: string; skill: string };
}

/** Why a save or a check was refused, as `refused` in `recipes.rs` details it. */
export type RecipesRefusal =
  | { kind: 'stale' }
  | { kind: 'no_home' }
  | { kind: 'invalid'; key: string | null; message: string }
  | { kind: 'untrusted'; path: string }
  | { kind: 'config'; message: string }
  | { kind: 'unremovable'; key: string; from: string }
  | { kind: 'not_a_table'; key: string }
  | { kind: 'unparsed'; key: string; message: string }
  | { kind: 'unreadable'; message: string };

export const PROVIDERS: { id: RoleProvider; name: string }[] = [
  { id: 'claude', name: 'Claude' },
  { id: 'codex', name: 'Codex' },
];

export const INPUTS: { id: RoleInput; label: string }[] = [
  { id: 'diff', label: 'Diff' },
  { id: 'rules', label: 'Your rules' },
  { id: 'instructions', label: 'Repository instructions' },
  { id: 'findings', label: 'Earlier findings' },
  { id: 'issue', label: 'Issue' },
  { id: 'plan', label: 'Plan' },
  { id: 'design', label: 'Design' },
  { id: 'answers', label: 'Your answers' },
];

export const HOME_WORDS: Record<WorkflowHome, { label: string; help: string }> = {
  local: {
    label: 'This repository, on this Mac',
    help: 'The untracked local layer inside .git, shared by every worktree of the repository and never committed.',
  },
  project: {
    label: 'This project, in your own config',
    help: "The project's entry in your own config. Other projects are unaffected.",
  },
  defaults: {
    label: 'Every project',
    help: 'Your defaults, which every project reads unless it says otherwise.',
  },
};

/** Role and recipe ids end up in keys, logs and directory names: the config's own rule. */
export const ID_PATTERN = /^[A-Za-z0-9_-]+$/;

/** The default `timeout_ms`, 30 minutes, as `wtm_core::flow::recipe::DEFAULT_TIMEOUT_MS`. */
export const DEFAULT_TIMEOUT_MS = 30 * 60 * 1000;
export const MAX_RETRIES = 3;
export const MAX_FALLBACKS = 3;

export function providerLabel(provider: RoleProvider): string {
  return PROVIDERS.find((p) => p.id === provider)?.name ?? provider;
}

/** A deep copy that is safe to edit, from a view or a `$state` proxy alike. */
export function draftOf(workflow: {
  role: Record<string, RoleSpec>;
  recipe: Record<string, RecipeSpec>;
}): WorkflowDraft {
  return JSON.parse(
    JSON.stringify({ role: workflow.role, recipe: workflow.recipe }),
  ) as WorkflowDraft;
}

/** Whether two drafts would write the same file. Key order is not a difference. */
export function sameDraft(a: WorkflowDraft, b: WorkflowDraft): boolean {
  return canonical(a) === canonical(b);
}

function canonical(value: unknown): string {
  return JSON.stringify(value, (_key, v: unknown) =>
    v && typeof v === 'object' && !Array.isArray(v)
      ? Object.fromEntries(
          Object.entries(v as Record<string, unknown>).sort(([x], [y]) =>
            x.localeCompare(y),
          ),
        )
      : v,
  );
}

export function recipeLabel(id: string, recipe: RecipeSpec | undefined): string {
  return recipe?.label?.trim() || id;
}

/** "Codex · gpt-6 · high": who runs a role. */
export function agentLine(role: RoleSpec): string {
  return [providerLabel(role.provider), role.model, role.effort]
    .filter(Boolean)
    .join(' · ');
}

/** A timeout in minutes as the editor shows it: `15`, `1.5`, `0.33`, never rounded to a whole. */
export function minutesOf(ms: number): string {
  return String(Number((ms / 60_000).toFixed(2)));
}

/** "15 min · 1 retry". */
export function limitsLine(role: RoleSpec): string {
  const minutes = minutesOf(role.timeout_ms ?? DEFAULT_TIMEOUT_MS);
  const retries = role.retries ?? 0;
  return `${minutes} min · ${retries === 0 ? 'no retry' : retries === 1 ? '1 retry' : `${retries} retries`}`;
}

export function fallbackRoles(role: RoleSpec): string[] {
  if (Array.isArray(role.fallback)) return role.fallback;
  return role.fallback === 'ask' ? [] : [role.fallback];
}

/** "Pause and ask me", or the roles tried instead and who runs them. */
export function fallbackLine(role: RoleSpec, roles: Record<string, RoleSpec>): string {
  const chain = fallbackRoles(role);
  if (chain.length === 0) return 'Pause and ask me';
  const named = chain.map((id) => {
    const target = roles[id];
    return target ? `\`${id}\` on ${agentLine(target)}` : `\`${id}\``;
  });
  return `Switch to ${named.join(', then ')}, then ask me`;
}

export function knowledgeLine(role: RoleSpec): string {
  const rules = role.inputs.includes('rules');
  const instructions = role.inputs.includes('instructions');
  const areas = role.knowledge?.areas ?? [];
  const cap = role.knowledge?.max_rules;
  const parts: string[] = [];
  if (rules) {
    parts.push(
      `${cap ? `Up to ${cap} rules` : 'Your rules'} on touched paths${areas.length > 0 ? ` and ${areas.join(', ')}` : ''}`,
    );
  }
  if (instructions) parts.push('the repository’s instructions');
  if (parts.length === 0) return 'None';
  const line = parts.join(' + ');
  return line.charAt(0).toUpperCase() + line.slice(1);
}

export function pathsLine(role: RoleSpec): string {
  const paths = role.paths ?? [];
  return paths.length === 0 ? 'All changed files' : paths.join(', ');
}

/** Comma- or newline-separated, trimmed, empties dropped. */
export function splitList(text: string): string[] {
  return text
    .split(/[\n,]/)
    .map((part) => part.trim())
    .filter((part) => part !== '');
}

/** What a new review role starts as: a reviewer that reads the diff and your guidance. */
export function newRole(provider: RoleProvider = 'claude'): RoleSpec {
  return {
    objective: '',
    instructions: null,
    provider,
    model: null,
    effort: null,
    skills: [],
    paths: [],
    checks: [],
    inputs: ['diff', 'rules', 'instructions'],
    output: 'findings',
    profile: 'reviewer',
    workspace: null,
    timeout_ms: null,
    retries: 1,
    fallback: 'ask',
  };
}

/**
 * A role moved to another provider. Its model and effort are the old provider's own spelling and
 * would not start on the new one, so they are cleared rather than carried over.
 */
export function onProvider(role: RoleSpec, provider: RoleProvider): RoleSpec {
  if (role.provider === provider) return role;
  return { ...role, provider, model: null, effort: null };
}

// ── where a role is used ──

export interface Reference {
  /** What refers to it, for the sentence. */
  text: string;
  /** Whether the editor can take the reference away with the role, or it has to change elsewhere. */
  removable: boolean;
}

/** Everything that names `id`: stages, other roles' fallbacks, and the issue roles. */
export function referencesTo(
  id: string,
  draft: WorkflowDraft,
  issue: {
    planner: string | null;
    designer: string | null;
    implement: ImplementSpec | null;
  },
): Reference[] {
  const out: Reference[] = [];
  if (issue.planner === id)
    out.push({ text: 'it is the planner (`planner` in [workflow])', removable: false });
  if (issue.designer === id)
    out.push({ text: 'it is the designer (`designer` in [workflow])', removable: false });
  if (issue.implement?.writer === id)
    out.push({ text: 'it is the implementer (`implement.writer`)', removable: false });
  for (const [recipeId, recipe] of Object.entries(draft.recipe)) {
    recipe.stage.forEach((stage, index) => {
      if (stage.roles.includes(id))
        out.push({
          text: `stage ${index + 1} of ${recipeLabel(recipeId, recipe)}`,
          removable: true,
        });
    });
  }
  for (const [other, role] of Object.entries(draft.role)) {
    if (other !== id && fallbackRoles(role).includes(id))
      out.push({ text: `\`${other}\`'s fallback`, removable: true });
  }
  return out;
}

// ── edits: each a new draft ──

function clone(draft: WorkflowDraft): WorkflowDraft {
  return draftOf(draft);
}

export function putRole(draft: WorkflowDraft, id: string, role: RoleSpec): WorkflowDraft {
  const next = clone(draft);
  next.role[id] = JSON.parse(JSON.stringify(role)) as RoleSpec;
  return next;
}

/** Delete a role, and take it out of every stage and fallback that names it. */
export function deleteRole(draft: WorkflowDraft, id: string): WorkflowDraft {
  const next = clone(draft);
  delete next.role[id];
  for (const recipe of Object.values(next.recipe)) {
    recipe.stage = recipe.stage
      .map((stage) => ({ roles: stage.roles.filter((r) => r !== id) }))
      .filter((stage) => stage.roles.length > 0);
  }
  for (const role of Object.values(next.role)) {
    const chain = fallbackRoles(role).filter((r) => r !== id);
    if (chain.length !== fallbackRoles(role).length)
      role.fallback =
        chain.length === 0 ? 'ask' : Array.isArray(role.fallback) ? chain : chain[0]!;
  }
  return next;
}

/** Put `role` into a recipe at `stage` (a new last stage when it is the stage count). */
export function addToStage(
  draft: WorkflowDraft,
  recipe: string,
  stage: number,
  role: string,
): WorkflowDraft {
  const next = clone(draft);
  const spec = next.recipe[recipe];
  if (!spec) return draft;
  if (stage >= spec.stage.length) spec.stage.push({ roles: [role] });
  else spec.stage[stage]!.roles.push(role);
  return next;
}

export function removeFromRecipe(
  draft: WorkflowDraft,
  recipe: string,
  stage: number,
  role: string,
): WorkflowDraft {
  const next = clone(draft);
  const spec = next.recipe[recipe];
  if (!spec?.stage[stage]) return draft;
  spec.stage[stage].roles = spec.stage[stage].roles.filter((r) => r !== role);
  // An emptied stage is left for you to fill or remove: deleting it here would renumber the stages
  // under the pointer, and validation says plainly that a stage needs a role.
  return next;
}

/**
 * Move a role one place earlier or later in a recipe, reading the stages as one list: past the end
 * of its stage it joins the next one, which is how a role moves between stages without a menu.
 */
export function moveRole(
  draft: WorkflowDraft,
  recipe: string,
  stage: number,
  role: string,
  by: -1 | 1,
): WorkflowDraft {
  const next = clone(draft);
  const spec = next.recipe[recipe];
  const roles = spec?.stage[stage]?.roles;
  if (!spec || !roles) return draft;
  const at = roles.indexOf(role);
  const to = at + by;
  if (to >= 0 && to < roles.length) {
    roles.splice(at, 1);
    roles.splice(to, 0, role);
    return next;
  }
  const neighbour = spec.stage[stage + by];
  if (!neighbour) return draft;
  roles.splice(at, 1);
  if (by < 0) neighbour.roles.push(role);
  else neighbour.roles.unshift(role);
  return next;
}

export function moveStage(
  draft: WorkflowDraft,
  recipe: string,
  stage: number,
  by: -1 | 1,
): WorkflowDraft {
  const next = clone(draft);
  const stages = next.recipe[recipe]?.stage;
  const to = stage + by;
  if (!stages || to < 0 || to >= stages.length) return draft;
  const [moved] = stages.splice(stage, 1);
  stages.splice(to, 0, moved!);
  return next;
}

export function removeStage(
  draft: WorkflowDraft,
  recipe: string,
  stage: number,
): WorkflowDraft {
  const next = clone(draft);
  next.recipe[recipe]?.stage.splice(stage, 1);
  return next;
}

/** A copy of `from` under a new id, its stages naming the same roles. */
export function duplicateRecipe(
  draft: WorkflowDraft,
  from: string,
  id: string,
  label: string,
): WorkflowDraft {
  const next = clone(draft);
  const source = next.recipe[from];
  if (!source) return draft;
  next.recipe[id] = {
    label: label.trim() || null,
    stage: source.stage.map((stage) => ({ roles: [...stage.roles] })),
    concurrency: source.concurrency,
  };
  return next;
}

/** A new recipe with one stage running `role`: a recipe needs at least one role to be valid. */
export function newRecipe(
  draft: WorkflowDraft,
  id: string,
  label: string,
  role: string,
): WorkflowDraft {
  const next = clone(draft);
  next.recipe[id] = {
    label: label.trim() || null,
    stage: [{ roles: [role] }],
    concurrency: null,
  };
  return next;
}

/** An unused id built from `base`: `standard-copy`, `standard-copy-2`. */
export function freeId(base: string, taken: Record<string, unknown>): string {
  if (!(base in taken)) return base;
  for (let n = 2; ; n += 1) if (!(`${base}-${n}` in taken)) return `${base}-${n}`;
}

// ── errors at their fields ──

/** Where a validation key points: a role's field, a recipe's stage, or the recipe itself. */
export type ErrorAt =
  | { at: 'role'; role: string; field: string | null }
  | { at: 'stage'; recipe: string; stage: number }
  | { at: 'recipe'; recipe: string; field: string | null }
  | { at: 'workflow'; field: string };

export function errorAt(key: string | null): ErrorAt | null {
  if (!key) return null;
  const role = /^role\.([A-Za-z0-9_-]+)(?:\.(.+))?$/.exec(key);
  if (role) return { at: 'role', role: role[1]!, field: role[2] ?? null };
  const stage = /^recipe\.([A-Za-z0-9_-]+)\.stage\[(\d+)\]/.exec(key);
  if (stage) return { at: 'stage', recipe: stage[1]!, stage: Number(stage[2]) };
  const recipe = /^recipe\.([A-Za-z0-9_-]+)(?:\.(.+))?$/.exec(key);
  if (recipe) return { at: 'recipe', recipe: recipe[1]!, field: recipe[2] ?? null };
  return { at: 'workflow', field: key };
}

/** The refusal's detail, when the error is one of ours. */
export function refusalOf(error: unknown): RecipesRefusal | null {
  if (typeof error !== 'object' || error === null) return null;
  const detail = (error as { kind?: unknown; detail?: unknown }).detail;
  if (
    (error as { kind?: unknown }).kind !== 'recipes' ||
    typeof detail !== 'object' ||
    detail === null
  )
    return null;
  return detail as RecipesRefusal;
}

/** The key a refusal is about, for placing it beside its field. */
export function refusalKey(refusal: RecipesRefusal | null): string | null {
  if (!refusal) return null;
  switch (refusal.kind) {
    case 'invalid':
      return refusal.key;
    case 'unremovable':
    case 'not_a_table':
    case 'unparsed':
      return refusal.key;
    default:
      return null;
  }
}

// ── skills, as Check recipe found them ──

export type SkillMark =
  /** The provider listed it. */
  | 'listed'
  /** The provider answered and did not list it. */
  | 'missing'
  /** The provider could not be asked. */
  | 'unasked'
  /** Not checked yet, or checked before the role changed. */
  | 'unchecked';

export function skillName(skill: string): string {
  return skill.trim().replace(/^\/+/, '');
}

export function skillMark(
  role: RoleSpec,
  skill: string,
  check: SkillCheckView | null,
): SkillMark {
  if (check?.reported)
    return skillName(skill) === check.reported.skill ? 'missing' : 'unchecked';
  const answer = check?.providers.find((p) => p.provider === role.provider);
  if (!answer) return 'unchecked';
  if (answer.listed === null) return 'unasked';
  return answer.listed.includes(skillName(skill)) ? 'listed' : 'missing';
}

/** "✓ listed by Codex", "✗ not listed by Claude here". */
export function skillMarkWords(mark: SkillMark, provider: RoleProvider): string {
  const name = providerLabel(provider);
  switch (mark) {
    case 'listed':
      return `✓ listed by ${name}`;
    case 'missing':
      return `✗ not listed by ${name} here`;
    case 'unasked':
      return `${name} could not be asked`;
    case 'unchecked':
      return 'not checked yet';
  }
}

/** The skills of `role` its provider did not list, by the role's own spelling. */
export function missingSkills(role: RoleSpec, check: SkillCheckView | null): string[] {
  return (role.skills ?? []).filter((skill) => skillMark(role, skill, check) === 'missing');
}

/**
 * A check made against the role as it is now. A check is about one draft: after an edit to a role
 * its marks no longer describe it, so the role's marks are dropped rather than shown stale.
 */
export function checkStillHolds(
  checked: WorkflowDraft | null,
  draft: WorkflowDraft,
  role: string,
): boolean {
  if (!checked) return false;
  const a = checked.role[role];
  const b = draft.role[role];
  return (
    !!a &&
    !!b &&
    a.provider === b.provider &&
    canonical(a.skills ?? []) === canonical(b.skills ?? [])
  );
}

/** A skill problem a run reported, as a check would have found it: Claude did not list `/x`. */
export function checkFromProblem(problem: SkillProblem): SkillCheckView {
  return {
    roles: [problem.role],
    providers: [
      {
        provider: problem.provider,
        listed: problem.kind === 'missing' ? [] : null,
        problem:
          problem.kind === 'unlisted'
            ? 'it did not report its skills when the review ran'
            : null,
      },
    ],
    problems: [problem],
    reported:
      problem.kind === 'missing' ? { role: problem.role, skill: problem.skill } : undefined,
  };
}

/** The other provider, for "Move role to Codex". */
export function otherProvider(provider: RoleProvider): RoleProvider {
  return provider === 'claude' ? 'codex' : 'claude';
}

/** The roles a recipe runs, in order, with their stage. */
export function rolesOf(recipe: RecipeSpec | undefined): { role: string; stage: number }[] {
  return (recipe?.stage ?? []).flatMap((stage, index) =>
    stage.roles.map((role) => ({ role, stage: index })),
  );
}

/** The roles `role` may fall back to: the same profile, output and kind, as validation requires. */
export function fallbackChoices(
  id: string,
  role: RoleSpec,
  roles: Record<string, RoleSpec>,
): string[] {
  return Object.entries(roles)
    .filter(
      ([other, spec]) =>
        other !== id &&
        spec.profile === role.profile &&
        spec.output === role.output &&
        (spec.kind ?? 'static') === (role.kind ?? 'static'),
    )
    .map(([other]) => other);
}

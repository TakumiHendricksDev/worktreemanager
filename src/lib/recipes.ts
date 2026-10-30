/**
 * Recipes on the window's side: the moments that can offer them, and the starting points for new
 * ones. The shape and its rules live in Rust (`wtm_core::model::recipe`), which validates every save
 * and every start; nothing here is the last word on whether a recipe is runnable.
 *
 * # The action registry
 *
 * An action is a moment in wtm where a recipe can be *offered*. Binding recipes to one never runs
 * anything: the moment asks, and with nothing bound it asks nothing and behaves as it always did.
 *
 * Kept here rather than in Rust because everything about an action is about the window — where the
 * question appears, what the moment knows that the backend does not (the New Worktree form's values,
 * a setup's exit code). The backend stores bindings by id and treats the id as opaque, so an action
 * added by a newer build survives a write from this one.
 *
 * Adding an action is one entry below and one `<RecipeOffer>` (or `recipes.ask`) where the moment
 * happens. Nothing else enumerates them.
 */
import type { IconName } from './components/ui/icons';
import type { Recipe, RecipeStep } from './ipc/types';

export type RecipeActionId =
  'worktree.created' | 'worktree.setup_failed' | 'worktree.removing' | 'project.added';

export interface RecipeAction {
  id: RecipeActionId;
  name: string;
  icon: IconName;
  /** When the moment happens, in a sentence. */
  when: string;
  /** Where the question appears, in a sentence. */
  where: string;
  /** Variables a recipe offered here can use, beyond `worktree.*` and `project.*`. */
  variables: string[];
}

/** Variables every run has, whatever offered it. Filled by the backend from where the run is. */
export const PLACE_VARIABLES = [
  'worktree.issue',
  'worktree.name',
  'worktree.branch',
  'worktree.path',
  'project.name',
  'project.path',
];

export const RECIPE_ACTIONS: readonly RecipeAction[] = [
  {
    id: 'worktree.created',
    name: 'Worktree created',
    icon: 'plus',
    when: 'After New Worktree finishes and its setup command succeeds.',
    where: 'On the New Worktree pane’s finished screen, under the steps.',
    variables: ['form.*'],
  },
  {
    id: 'worktree.setup_failed',
    name: 'Setup failed',
    icon: 'warn',
    when: 'When a new worktree’s setup command fails or times out. The worktree is kept, as it is today.',
    where: 'On the same screen, beside Retry setup.',
    variables: ['form.*', 'action.outcome', 'action.exit_code'],
  },
  {
    id: 'worktree.removing',
    name: 'Removing a worktree',
    icon: 'trash',
    when: 'When you choose Remove worktree…, before anything is deleted.',
    where:
      'In the Remove dialog. A recipe runs first; the dialog comes back when it finishes, and nothing is removed until you press Remove.',
    variables: [],
  },
  {
    id: 'project.added',
    name: 'Repository added',
    icon: 'folder',
    when: 'After Add a repository… registers a repository.',
    where: 'In a notice at the bottom right. Runs in the main checkout.',
    variables: [],
  },
];

export function actionById(id: string): RecipeAction | undefined {
  return RECIPE_ACTIONS.find((action) => action.id === id);
}

/**
 * The prefix every recipe run id starts with. Mirrors `recipes::RUN_PREFIX`.
 *
 * How a pane tells it was opened by a recipe step rather than by a delegation, without a field on
 * every pane that is null for nearly all of them.
 */
export const RUN_PREFIX = 'recipe-';

export function isRecipeRun(run: string | null | undefined): boolean {
  return typeof run === 'string' && run.startsWith(RUN_PREFIX);
}

/**
 * The two letters that stand for an agent in a tight row. Not the first two of its label, which
 * would make Codex "Co" beside Claude's "Cl" — two marks that differ in one letter of two.
 */
export function agentMark(agent: string): string {
  return { claude: 'Cl', codex: 'Cx', cursor: 'Cu' }[agent] ?? agent.slice(0, 2);
}

/** What `settingsView.focus` says to open Settings › Recipes on a new, unsaved recipe. */
export const NEW_RECIPE = '#new';

export function blankStep(agent = 'claude'): RecipeStep {
  return {
    name: '',
    agent,
    model: null,
    effort: null,
    mode: null,
    kind: 'prompt',
    skill: '',
    args: '',
    prompt: '',
    conversation: 'new',
    previousReply: false,
    onFailure: 'stop',
  };
}

export function blankRecipe(): Recipe {
  return {
    id: '',
    name: 'Untitled recipe',
    description: '',
    repositories: [],
    steps: [{ ...blankStep(), name: 'Step 1' }],
  };
}

/**
 * Starting points, offered under the recipe list.
 *
 * Models are left to each provider's default and no skill is assumed beyond the CLIs' own, because
 * a template has to run on a machine with nothing installed but the agent.
 */
export const TEMPLATES: readonly { id: string; build: () => Recipe }[] = [
  {
    id: 'review',
    build: () => ({
      id: '',
      name: 'Review',
      description: 'Two independent reviews of the working tree.',
      repositories: [],
      steps: [
        {
          ...blankStep('claude'),
          name: 'Correctness',
          mode: 'plan',
          prompt:
            'Review the changes on {{worktree.branch}} for correctness bugs: wrong logic, missed edge cases, broken error handling. List each finding with the file and line. Change nothing.',
        },
        {
          ...blankStep('codex'),
          name: 'Second opinion',
          mode: 'read-only',
          kind: 'skill',
          skill: 'review',
          previousReply: true,
        },
      ],
    }),
  },
  {
    id: 'implementation',
    build: () => ({
      id: '',
      name: 'Implementation',
      description: 'Plan the work, build it, then have Codex review it.',
      repositories: [],
      steps: [
        {
          ...blankStep('claude'),
          name: 'Plan',
          mode: 'plan',
          prompt:
            'Plan the work for {{worktree.branch}}. List the files you will change and the tests you will add. Change nothing yet.',
        },
        {
          ...blankStep('claude'),
          name: 'Implement',
          mode: 'auto',
          conversation: 'continue',
          prompt:
            'Implement the plan. Run the project’s checks before you finish and say what you could not verify.',
        },
        {
          ...blankStep('codex'),
          name: 'Review',
          mode: 'read-only',
          kind: 'skill',
          skill: 'review',
          previousReply: true,
        },
      ],
    }),
  },
];

/**
 * Whether step `index` may continue the previous step's conversation: only on the same agent.
 *
 * Mirrors the rule `Recipe::problems` enforces, so the editor can disable the choice and say why
 * instead of letting someone pick it and be refused at Save.
 */
export function canContinue(steps: readonly RecipeStep[], index: number): boolean {
  const previous = steps[index - 1];
  return previous !== undefined && previous.agent === steps[index]?.agent;
}

/**
 * Variables the editor offers to insert, in the order people reach for them. The repository's own
 * New Worktree fields are offered beside these as `form.<key>`, read from its form rather than
 * guessed — a `form.ticket` chip offered to a repository whose field is `issue` is how a recipe
 * came to send an empty ticket.
 */
export const INSERTABLE = [
  'worktree.issue',
  'worktree.branch',
  'worktree.name',
  'project.name',
  'previous.reply',
];

/** The variables a step's text uses that will not resolve, and why, for the editor to warn about. */
export function unresolved(
  text: string,
  formKeys: readonly string[] | null,
): { name: string; why: string }[] {
  const out: { name: string; why: string }[] = [];
  for (const match of text.matchAll(/\{\{\s*([a-z_]+)\.([a-zA-Z0-9_.*-]+)\s*\}\}/g)) {
    const [, namespace, key] = match;
    const name = `${namespace}.${key}`;
    if (out.some((u) => u.name === name)) continue;
    if (namespace === 'worktree' || namespace === 'project') {
      if (!PLACE_VARIABLES.includes(name))
        out.push({
          name,
          why: `there is no ${name}; there is ${PLACE_VARIABLES.filter((v) => v.startsWith(namespace)).join(', ')}`,
        });
    } else if (namespace === 'previous' && key !== 'reply') {
      out.push({ name, why: 'the only one is previous.reply' });
    } else if (namespace === 'form' && formKeys !== null && !formKeys.includes(key ?? '')) {
      out.push({
        name,
        why:
          formKeys.length > 0
            ? `this repository’s New Worktree form has no “${key}” field; it has ${formKeys.join(', ')}`
            : 'this repository’s New Worktree form has no fields',
      });
    }
  }
  return out;
}

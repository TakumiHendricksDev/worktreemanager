<script lang="ts">
  /**
   * Remove a worktree.
   *
   * The shell script asks `Delete branch <x>? [y/n]` on stdin, with no `--yes` flag and a
   * `confirm()` helper that loops forever on EOF. wtm runs git itself, so that question becomes
   * the checkbox below — which is the entire reason `[remove] strategy` defaults to `native`.
   *
   * Preflight runs on open and again when the branch checkbox changes, because "this branch has
   * commits not in the base" only matters if you are deleting it. Nothing here mutates until
   * Remove is pressed.
   *
   * Remove closes the dialog rather than waiting in it: teardown can take minutes, and this is
   * the decision, not the work. `worktreeRemoval.run` carries the removal on and reports how it
   * ended — see the note there.
   */
  import { commands } from '../ipc/commands';
  import { errorMessage, type Preflight, type Worktree } from '../ipc/types';
  import { recipes } from '../state/recipes.svelte';
  import { sessions } from '../state/sessions.svelte';
  import { worktreeRemoval } from '../state/worktree-removal.svelte';
  import Button from './ui/Button.svelte';
  import Choice from './ui/Choice.svelte';
  import Dialog from './ui/Dialog.svelte';
  import Icon from './ui/Icon.svelte';
  import PreflightList from './ui/PreflightList.svelte';

  const {
    projectId,
    worktree,
    onclose,
  }: { projectId: string; worktree: Worktree; onclose: () => void } = $props();

  let deleteBranch = $state(false);
  let force = $state(false);
  let preflight = $state<Preflight[]>([]);
  let loading = $state(true);
  let preflightReady = $state(false);
  let error = $state<string | null>(null);
  const acknowledged = $state<string[]>([]);

  const affected = $derived(
    sessions.panes.filter(
      (pane) => pane.projectId === projectId && pane.worktreeId === worktree.id,
    ),
  );

  const errors = $derived(preflight.filter((p) => p.severity === 'error'));
  const warns = $derived(preflight.filter((p) => p.severity === 'warn'));

  const canRemove = $derived(
    !loading &&
      preflightReady &&
      errors.every((p) => p.overridable && (force || acknowledged.includes(p.id))),
  );

  // Re-check when the branch checkbox changes: the unmerged-work warning only applies then.
  let previewSeq = 0;
  $effect(() => {
    const wantsBranch = deleteBranch;
    const seq = ++previewSeq;
    loading = true;
    preflightReady = false;
    void commands
      .removePreflight(projectId, worktree.id, wantsBranch)
      .then((items) => {
        if (seq !== previewSeq) return;
        preflight = items;
        preflightReady = true;
        error = null;
      })
      .catch((e) => {
        if (seq !== previewSeq) return;
        preflightReady = false;
        error = errorMessage(e);
      })
      .finally(() => {
        if (seq === previewSeq) loading = false;
      });
  });

  function toggleAck(id: string, on: boolean) {
    const index = acknowledged.indexOf(id);
    if (on && index === -1) acknowledged.push(id);
    if (!on && index !== -1) acknowledged.splice(index, 1);
  }

  /*
   * "Removing a worktree" can offer recipes — a wrap-up that says what would be lost, say. Choosing
   * one runs it *instead of* removing for now: the dialog closes, the recipe runs, and this dialog
   * comes back when it finishes so the removal gets a second look with the answer on screen. Nothing
   * here ever removes after a recipe on its own.
   */
  const offered = $derived(
    recipes.isRunningIn(worktree.id)
      ? []
      : recipes.offeredFor('worktree.removing', projectId),
  );
  let picked = $state<string | null>(null);
  const pick = $derived(offered.find((r) => r.id === picked) ?? offered[0] ?? null);
  let starting = $state(false);

  async function runFirst() {
    if (!pick || starting) return;
    starting = true;
    try {
      await recipes.start({
        recipeId: pick.id,
        projectId,
        worktreeId: worktree.id,
        action: 'worktree.removing',
        then: () => void worktreeRemoval.request({ projectId, worktree }),
      });
      onclose();
    } catch {
      // Said in a notice by `recipes.start`; the dialog stays so Remove is still one click away.
    } finally {
      starting = false;
    }
  }

  function remove() {
    if (!canRemove) return;
    void worktreeRemoval.run(
      { projectId, worktree },
      { deleteBranch, force, acknowledged: [...acknowledged] },
    );
    onclose();
  }
</script>

<Dialog title="Remove this worktree?" {onclose} wide>
  {#snippet body()}
    <p>
      This ends the worktree’s agents, shells and browser panes before running teardown.
      {#if affected.length > 0}{affected.length} open {affected.length === 1
          ? 'pane will close'
          : 'panes will close'}.{/if}
    </p>
    <p class="c-remove__target"><code>{worktree.path}</code></p>
    {#if worktree.branch}
      <p class="c-remove__sub">on <code>{worktree.branch}</code></p>
    {:else}
      <p class="c-remove__sub c-status--muted">detached HEAD</p>
    {/if}

    <Choice
      checked={deleteBranch}
      disabled={!worktree.branch}
      onchange={(on) => (deleteBranch = on)}
    >
      Also delete the branch
      {#if worktree.branch}<code>{worktree.branch}</code>{/if}
    </Choice>

    {#if loading}
      <p class="c-status--muted">Checking…</p>
    {:else}
      <PreflightList
        items={[...errors, ...warns]}
        acknowledged={force ? errors.map((e) => e.id) : acknowledged}
        overrideLabel="Remove anyway"
        disabled={force}
        onacknowledge={toggleAck}
      />
    {/if}

    <Choice size="sm" checked={force} onchange={(on) => (force = on)}>
      Force — discard uncommitted and untracked files
    </Choice>

    <p class="c-note">
      The project's teardown steps run first, so containers are stopped and root-owned files
      are handed back before git touches the directory.
    </p>

    {#if pick}
      <div class="c-before" data-od-id="remove-offer">
        <p class="c-before__head">
          <Icon name="recipe" />
          {offered.length === 1 ? `Run ${pick.name} first?` : 'Run a recipe first?'}
        </p>
        <p class="c-before__text">
          Removing a worktree offers {offered.length === 1 ? 'this' : 'these'}. It runs
          first; this dialog comes back when it finishes, and nothing is removed until you
          press Remove.
        </p>
        <div class="c-before__actions">
          {#if offered.length > 1}
            <select
              class="c-select c-before__select"
              aria-label="Recipe to run first"
              value={pick.id}
              onchange={(e) => (picked = (e.currentTarget as HTMLSelectElement).value)}
            >
              {#each offered as recipe (recipe.id)}
                <option value={recipe.id}>{recipe.name}</option>
              {/each}
            </select>
          {/if}
          <Button
            variant="neutral"
            size="sm"
            disabled={starting}
            onclick={() => void runFirst()}
          >
            <Icon name="play" size={12} />
            {starting ? 'Starting…' : `Run ${pick.name} first`}
          </Button>
        </div>
      </div>
    {/if}

    {#if error}
      <p class="c-status--danger">{error}</p>
    {/if}
  {/snippet}

  {#snippet footer()}
    <Button variant="neutral" onclick={onclose}>Cancel</Button>
    <Button variant="danger-solid" onclick={remove} disabled={!canRemove}>Remove</Button>
  {/snippet}
</Dialog>

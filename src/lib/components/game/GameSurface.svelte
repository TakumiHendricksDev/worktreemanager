<script lang="ts">
  /**
   * The optional world overview.
   *
   * This component never owns a worktree operation. Its buttons either call an existing command or
   * hand navigation back to `App`, which reveals the ordinary, still-mounted workbench. That keeps
   * game mode an alternate projection rather than a second implementation of WTM.
   */
  import { onMount, untrack } from 'svelte';

  import { buildGameSnapshot, type GameActor, type GameJob } from '../../game/model';
  import type { WorldPick, WorldRuntime, WorldStats } from '../../game/runtime';
  import { commands } from '../../ipc/commands';
  import { gameWorld } from '../../state/game-world.svelte';
  import { sessions } from '../../state/sessions.svelte';
  import { workspace } from '../../state/workspace.svelte';
  import { STATUS_WORD } from '../../status';
  import Banner from '../ui/Banner.svelte';
  import Button from '../ui/Button.svelte';
  import Icon from '../ui/Icon.svelte';
  import GameSettingsDialog from './GameSettingsDialog.svelte';

  type Destination = 'sessions' | 'database';

  const {
    visible,
    onstandard,
    onsettings,
    onaddproject,
    onnavigate,
    onnewworktree,
    onremove,
    oninspect,
    onfavorite,
  }: {
    visible: boolean;
    onstandard: () => void;
    onsettings: () => void;
    onaddproject: () => void;
    onnavigate: (
      projectId: string,
      worktreeId: string,
      destination: Destination,
    ) => Promise<void>;
    onnewworktree: (projectId: string) => Promise<void>;
    onremove: (projectId: string, worktreeId: string) => Promise<void>;
    oninspect: (projectId: string, worktreeId: string) => Promise<void>;
    onfavorite: (projectId: string, worktreeId: string) => Promise<void>;
  } = $props();

  let host: HTMLDivElement;
  let runtime = $state.raw<WorldRuntime | null>(null);
  let ready = $state(false);
  let worldError = $state<string | null>(null);
  let actionError = $state<string | null>(null);
  let showGameSettings = $state(false);
  let orbiting = $state(false);
  let selectedRepositoryId = $state<string | null>(null);
  let selectedJobId = $state<string | null>(null);
  let selectedActorId = $state<string | null>(null);
  let openerChoice = $state('');
  let linkChoice = $state('');
  let agentChoice = $state('');
  let stats = $state<WorldStats>({
    repositories: 0,
    agents: 0,
    working: 0,
    waiting: 0,
    blocked: 0,
    fps: 0,
  });
  let wasVisible = false;

  const snapshot = $derived(
    buildGameSnapshot(
      gameWorld.repositories,
      sessions.panes,
      (pane) => sessions.statusOfPane(pane),
      (pane) => sessions.labelOf(pane),
    ),
  );
  const selectedRepository = $derived(
    snapshot.repositories.find((repository) => repository.id === selectedRepositoryId) ??
      null,
  );
  const selectedJob = $derived(
    selectedRepository?.jobs.find((job) => job.id === selectedJobId) ?? null,
  );
  const selectedActor = $derived(
    selectedJob?.actors.find((actor) => actor.id === selectedActorId) ?? null,
  );
  const startable = $derived(
    sessions.options.filter((option) => option.available && option.offered),
  );
  const preferredOpener = $derived(
    workspace.openers.find((opener) => opener.id === workspace.preferredOpener) ??
      workspace.openers.find((opener) => opener.available) ??
      null,
  );

  onMount(() => {
    let disposed = false;
    // Three.js and the vendored renderer are a separate production chunk. A user who leaves the
    // beta off never downloads, parses, or initializes the game dependency graph.
    void import('../../game/runtime')
      .then(({ WorldRuntime }) => {
        if (disposed) return;
        try {
          runtime = new WorldRuntime(host, {
            onpick: handleWorldPick,
            onready: () => (ready = true),
            onerror: (message) => (worldError = message),
            onstats: (next) => (stats = next),
          });
        } catch (cause) {
          failWorld(cause);
        }
      })
      .catch((cause) => {
        if (!disposed) failWorld(cause);
      });

    const onFocus = () => {
      if (visible) void gameWorld.refresh();
    };
    window.addEventListener('focus', onFocus);
    return () => {
      disposed = true;
      window.removeEventListener('focus', onFocus);
      runtime?.dispose();
      runtime = null;
    };
  });

  function failWorld(cause: unknown): void {
    worldError =
      cause instanceof Error
        ? `Game Mode could not start: ${cause.message}`
        : 'Game Mode could not start on this display.';
    ready = true;
  }

  $effect(() => {
    const current = snapshot;
    const renderer = runtime;
    if (renderer) untrack(() => renderer.setSnapshot(current));
  });

  $effect(() => {
    runtime?.setVisible(visible);
    if (visible && !wasVisible) untrack(() => void gameWorld.refresh());
    wasVisible = visible;
  });

  // A project add/remove is rare and user-driven. Refresh once for the new catalogue rather than
  // carrying another copy of workspace's mutation logic into the world store.
  $effect(() => {
    const catalogue = workspace.projects.map((project) => project.id).join('\u0000');
    if (!catalogue && workspace.projects.length === 0) return;
    if (visible) untrack(() => void gameWorld.refresh());
  });

  // If a removal or project switch invalidates the selection, back out to the nearest real level.
  $effect(() => {
    if (selectedRepositoryId && !selectedRepository) clearSelection();
    else if (selectedJobId && !selectedJob) selectRepository(selectedRepositoryId);
    else if (selectedActorId && !selectedActor)
      selectJob(selectedRepositoryId, selectedJobId);
  });

  onMount(() => {
    const onKey = (event: KeyboardEvent) => {
      if (!visible || document.querySelector('[aria-modal="true"]')) return;
      const target = event.target;
      if (
        target instanceof HTMLInputElement ||
        target instanceof HTMLSelectElement ||
        target instanceof HTMLTextAreaElement
      )
        return;
      if (event.metaKey || event.ctrlKey || event.altKey) return;

      if (event.key === 'Escape') {
        if (selectedActorId) selectJob(selectedRepositoryId, selectedJobId);
        else if (selectedJobId) selectRepository(selectedRepositoryId);
        else clearSelection();
      } else if (event.key === '0') {
        runtime?.resetView();
      } else if (event.key.toLowerCase() === 'o') {
        orbiting = runtime?.toggleOrbit() ?? false;
      } else if (event.key.toLowerCase() === 'p') {
        runtime?.screenshot();
      } else if (event.key.toLowerCase() === 'n') {
        focusNextAttention();
      } else if (event.key.toLowerCase() === 'j' && selectedJob) {
        void openShell(selectedJob);
      }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  });

  function handleWorldPick(pick: WorldPick): void {
    if (pick.kind === 'ground') {
      clearSelection();
      return;
    }
    if (pick.kind === 'repository') {
      selectRepository(pick.id);
      return;
    }
    if (pick.kind === 'job') {
      for (const repository of snapshot.repositories) {
        if (repository.jobs.some((job) => job.id === pick.id)) {
          selectJob(repository.id, pick.id);
          return;
        }
      }
      return;
    }
    for (const repository of snapshot.repositories) {
      for (const job of repository.jobs) {
        if (job.actors.some((actor) => actor.id === pick.id)) {
          selectActor(repository.id, job.id, pick.id);
          return;
        }
      }
    }
  }

  function clearSelection(): void {
    selectedRepositoryId = null;
    selectedJobId = null;
    selectedActorId = null;
    runtime?.selectActor(null);
  }

  function selectRepository(projectId: string | null): void {
    if (!projectId) {
      clearSelection();
      return;
    }
    selectedRepositoryId = projectId;
    selectedJobId = null;
    selectedActorId = null;
    runtime?.selectActor(null);
    runtime?.focusRepository(projectId);
  }

  function selectJob(projectId: string | null, worktreeId: string | null): void {
    if (!projectId || !worktreeId) {
      selectRepository(projectId);
      return;
    }
    selectedRepositoryId = projectId;
    selectedJobId = worktreeId;
    selectedActorId = null;
    runtime?.selectActor(null);
    runtime?.focusJob(worktreeId);
    void sessions.refreshOptions(projectId);
  }

  function selectActor(projectId: string, worktreeId: string, actorId: string): void {
    selectedRepositoryId = projectId;
    selectedJobId = worktreeId;
    selectedActorId = actorId;
    runtime?.focusActor(actorId);
    void sessions.refreshOptions(projectId);
  }

  function focusNextAttention(): void {
    const actors = snapshot.repositories.flatMap((repository) =>
      repository.jobs.flatMap((job) => job.actors),
    );
    const urgent = actors.filter(
      (actor) => actor.paneStatus === 'attention' || actor.paneStatus === 'failed',
    );
    if (urgent.length === 0) return;
    const at = urgent.findIndex((actor) => actor.id === selectedActorId);
    const next = urgent[(at + 1) % urgent.length];
    if (next) selectActor(next.projectId, next.worktreeId, next.id);
  }

  async function enterJob(
    job: GameJob,
    destination: Destination = 'sessions',
  ): Promise<void> {
    actionError = null;
    try {
      await onnavigate(job.projectId, job.id, destination);
    } catch (cause) {
      actionError = cause instanceof Error ? cause.message : String(cause);
    }
  }

  async function openShell(job: GameJob): Promise<void> {
    await enterJob(job);
    await sessions.focusOrOpenShell(job.projectId, job.id);
  }

  async function openActor(actor: GameActor): Promise<void> {
    const job = selectedJob;
    if (!job) return;
    await enterJob(job);
    sessions.showRelated(actor.paneId);
  }

  async function pickAgent(event: Event): Promise<void> {
    const provider = (event.currentTarget as HTMLSelectElement).value;
    agentChoice = '';
    if (!provider || !selectedJob) return;
    const job = selectedJob;
    await enterJob(job);
    await sessions.openAgent(job.projectId, job.id, provider);
  }

  async function pickOpener(event: Event): Promise<void> {
    const id = (event.currentTarget as HTMLSelectElement).value;
    openerChoice = '';
    if (!id || !selectedJob) return;
    const opener = workspace.openers.find((option) => option.id === id);
    if (!opener?.available) return;
    actionError = null;
    try {
      await workspace.setPreferredOpener(id);
      await commands.openIn(selectedJob.projectId, selectedJob.id, id);
    } catch (cause) {
      actionError = cause instanceof Error ? cause.message : String(cause);
    }
  }

  async function openPreferred(): Promise<void> {
    if (!selectedJob || !preferredOpener?.available) return;
    actionError = null;
    try {
      await commands.openIn(selectedJob.projectId, selectedJob.id, preferredOpener.id);
    } catch (cause) {
      actionError = cause instanceof Error ? cause.message : String(cause);
    }
  }

  async function pickLink(event: Event): Promise<void> {
    const url = (event.currentTarget as HTMLSelectElement).value;
    linkChoice = '';
    if (!url) return;
    try {
      await commands.openUrl(url);
    } catch (cause) {
      actionError = cause instanceof Error ? cause.message : String(cause);
    }
  }
</script>

<section
  class="c-game-world"
  class:is-hidden={!visible}
  aria-label="Repository world"
  aria-hidden={!visible}
>
  <div class="c-game-world__viewport" bind:this={host}></div>

  <div class="c-game-world__toolbar">
    <div class="c-game-world__brand">
      <span>World</span>
      <span class="c-badge c-badge--accent">Beta</span>
    </div>
    <div class="c-game-world__stats" aria-label="World status">
      <button onclick={focusNextAttention} title="Focus the next agent that needs you (N)">
        <span class="c-game-world__pip c-game-world__pip--attention"></span>
        {stats.waiting} need you
      </button>
      <span
        ><span class="c-game-world__pip c-game-world__pip--working"></span>{stats.working} working</span
      >
      <span
        ><span class="c-game-world__pip c-game-world__pip--failed"></span>{stats.blocked} failed</span
      >
      <span>{stats.agents} bots</span>
    </div>
    <div class="c-game-world__tools">
      <Button
        variant="quiet"
        size="sm"
        onclick={() => runtime?.resetView()}
        title="Reset view (0)">Reset</Button
      >
      <Button
        variant={orbiting ? 'neutral' : 'quiet'}
        size="sm"
        ariaPressed={orbiting}
        onclick={() => (orbiting = runtime?.toggleOrbit() ?? false)}
        title="Orbit (O)">Orbit</Button
      >
      <Button
        variant="quiet"
        size="sm"
        onclick={() => runtime?.screenshot()}
        title="Save screenshot (P)">Screenshot</Button
      >
      <Button variant="quiet" size="sm" onclick={() => (showGameSettings = true)}
        >World settings</Button
      >
      <Button variant="neutral" size="sm" onclick={onstandard}>Standard view</Button>
    </div>
  </div>

  {#if !ready && !worldError}
    <div class="c-game-world__boot" role="status">
      <div class="c-game-world__spinner"></div>
      <p>Building your repository world…</p>
    </div>
  {/if}

  {#if worldError}
    <div class="c-game-world__notice">
      <Banner variant="warn">
        {worldError}
        {#snippet action()}
          <Button variant="inline" onclick={onstandard}>Use standard view</Button>
        {/snippet}
      </Banner>
    </div>
  {:else if gameWorld.error || actionError}
    <div class="c-game-world__notice">
      <Banner variant="warn">
        {actionError ?? gameWorld.error}
        {#snippet action()}
          <Button
            variant="inline"
            onclick={() => {
              actionError = null;
              void gameWorld.refresh();
            }}>Refresh</Button
          >
        {/snippet}
      </Banner>
    </div>
  {/if}

  <aside class="c-game-world__panel" aria-label="World navigator">
    {#if selectedActor && selectedJob && selectedRepository}
      <button
        class="c-game-world__back"
        onclick={() => selectJob(selectedRepository.id, selectedJob.id)}
      >
        <Icon name="chevron-left" size={13} />
        {selectedJob.worktree.title}
      </button>
      <div class="c-game-world__heading">
        <span class="c-game-world__avatar"
          >{selectedActor.label.slice(0, 1).toUpperCase()}</span
        >
        <div>
          <h2>{selectedActor.title}</h2>
          <p>
            {selectedActor.label}{selectedActor.model ? ` · ${selectedActor.model}` : ''}
          </p>
        </div>
      </div>
      <div class="c-game-world__status">
        <span class="c-game-world__pip c-game-world__pip--{selectedActor.paneStatus}"
        ></span>
        {STATUS_WORD[selectedActor.paneStatus] || 'idle'}
      </div>
      <Button variant="accent" size="lg" onclick={() => void openActor(selectedActor)}>
        Open session
      </Button>
      <Button variant="neutral" onclick={() => void enterJob(selectedJob)}
        >Open job workbench</Button
      >
    {:else if selectedJob && selectedRepository}
      <button
        class="c-game-world__back"
        onclick={() => selectRepository(selectedRepository.id)}
      >
        <Icon name="chevron-left" size={13} />
        {selectedRepository.name}
      </button>
      <div class="c-game-world__heading">
        <div>
          <h2>{selectedJob.worktree.title}</h2>
          <p>{selectedJob.worktree.branch ?? 'Detached worktree'}</p>
        </div>
        {#if selectedJob.worktree.isMain}<span class="c-badge">Headquarters</span>{/if}
      </div>

      <div class="c-game-world__facts">
        {#if selectedJob.worktree.dirty || selectedJob.worktree.staged || selectedJob.worktree.untracked}
          <span class="c-status--warn">Modified</span>
        {:else}
          <span class="c-status--ok">Clean</span>
        {/if}
        {#if selectedJob.worktree.ahead}<span>↑{selectedJob.worktree.ahead}</span>{/if}
        {#if selectedJob.worktree.behind}<span>↓{selectedJob.worktree.behind}</span>{/if}
        <span>{selectedJob.actors.length} bots</span>
        <span>{selectedJob.shells} consoles</span>
      </div>

      <div class="c-game-world__primary-actions">
        <Button variant="accent" onclick={() => void enterJob(selectedJob)}
          >Open workbench</Button
        >
        <Button variant="neutral" onclick={() => void openShell(selectedJob)}>
          <Icon name="terminal" size={13} /> Shell
        </Button>
      </div>

      {#if startable.length > 0}
        <label class="c-game-world__select">
          <span>Start an agent</span>
          <select bind:value={agentChoice} onchange={(event) => void pickAgent(event)}>
            <option value="">Choose agent…</option>
            {#each startable as option (option.id)}
              <option value={option.id}>{option.label}</option>
            {/each}
          </select>
        </label>
      {/if}

      {#if selectedJob.actors.length > 0}
        <div class="c-game-world__section">
          <h3>Bots on this job</h3>
          <div class="c-game-world__list">
            {#each selectedJob.actors as actor (actor.id)}
              <button
                onclick={() => selectActor(selectedRepository.id, selectedJob.id, actor.id)}
              >
                <span class="c-game-world__pip c-game-world__pip--{actor.paneStatus}"
                ></span>
                <span>{actor.title}</span>
                <small>{STATUS_WORD[actor.paneStatus] || actor.label}</small>
              </button>
            {/each}
          </div>
        </div>
      {/if}

      <div class="c-game-world__secondary-actions">
        <Button
          variant="quiet"
          size="sm"
          onclick={() => void oninspect(selectedJob.projectId, selectedJob.id)}
        >
          Details
        </Button>
        <Button
          variant="quiet"
          size="sm"
          onclick={() => void enterJob(selectedJob, 'database')}
        >
          Database
        </Button>
        <Button
          variant="quiet"
          size="sm"
          onclick={() => void onfavorite(selectedJob.projectId, selectedJob.id)}
        >
          {selectedJob.worktree.favorite ? 'Unfavorite' : 'Favorite'}
        </Button>
      </div>

      {#if preferredOpener}
        <div class="c-game-world__split-action">
          <Button
            variant="neutral"
            size="sm"
            onclick={() => void openPreferred()}
            disabled={!preferredOpener.available}
          >
            Open in {preferredOpener.label}
          </Button>
          <select
            bind:value={openerChoice}
            onchange={(event) => void pickOpener(event)}
            aria-label="Open in another application"
          >
            <option value="">Other…</option>
            {#each workspace.openers as opener (opener.id)}
              <option value={opener.id} disabled={!opener.available}>
                {opener.label}{opener.available ? '' : ' (not installed)'}
              </option>
            {/each}
          </select>
        </div>
      {/if}

      {#if selectedJob.worktree.links.length > 0}
        <label class="c-game-world__select">
          <span>Project links</span>
          <select bind:value={linkChoice} onchange={(event) => void pickLink(event)}>
            <option value="">Choose link…</option>
            {#each selectedJob.worktree.links as link, i (`${link.label}:${i}`)}
              <option value={link.url}>{link.label}</option>
            {/each}
          </select>
        </label>
      {/if}

      <Button
        variant="danger-outline"
        size="sm"
        disabled={selectedJob.worktree.isMain}
        onclick={() => void onremove(selectedJob.projectId, selectedJob.id)}
      >
        Remove worktree
      </Button>
    {:else if selectedRepository}
      <button class="c-game-world__back" onclick={clearSelection}>
        <Icon name="chevron-left" size={13} /> All repositories
      </button>
      <div class="c-game-world__heading">
        <div>
          <h2>{selectedRepository.name}</h2>
          <p title={selectedRepository.root}>{selectedRepository.root}</p>
        </div>
      </div>
      {#if selectedRepository.problem}
        <p class="c-status--warn">{selectedRepository.problem}</p>
      {/if}
      <Button
        variant="accent"
        disabled={!selectedRepository.usable}
        onclick={() => void onnewworktree(selectedRepository.id)}>New worktree</Button
      >
      <div class="c-game-world__section">
        <h3>Job sites</h3>
        <div class="c-game-world__list">
          {#each selectedRepository.jobs as job (job.id)}
            <button onclick={() => selectJob(selectedRepository.id, job.id)}>
              <span
                class="c-game-world__pip c-game-world__pip--{sessions.statuses[job.id] ??
                  'idle'}"
              ></span>
              <span>{job.worktree.title}</span>
              <small>{job.actors.length} bots · {job.shells} shells</small>
            </button>
          {:else}
            <p>No worktrees available.</p>
          {/each}
        </div>
      </div>
    {:else}
      <div class="c-game-world__heading">
        <div>
          <h2>Your repositories</h2>
          <p>{stats.repositories} islands · {stats.agents} bots</p>
        </div>
      </div>
      <div class="c-game-world__list">
        {#each snapshot.repositories as repository (repository.id)}
          {@const attention = repository.jobs.filter((job) =>
            job.actors.some(
              (actor) => actor.paneStatus === 'attention' || actor.paneStatus === 'failed',
            ),
          ).length}
          <button onclick={() => selectRepository(repository.id)}>
            <span class="c-game-world__island" aria-hidden="true"></span>
            <span>{repository.name}</span>
            <small>
              {repository.jobs.length} jobs{attention ? ` · ${attention} need you` : ''}
            </small>
          </button>
        {:else}
          <p>Add a repository to give the world its first island.</p>
        {/each}
      </div>
      <div class="c-game-world__panel-actions">
        <Button variant="accent" onclick={onaddproject}>Add repository</Button>
        <Button variant="quiet" onclick={onsettings}>WTM settings</Button>
      </div>
      <p class="c-game-world__hint">
        Drag to move · scroll to zoom · click an island, job, or bot
      </p>
    {/if}
  </aside>

  <div class="c-game-world__keys" aria-hidden="true">
    N next request · J shell · O orbit · P screenshot · 0 reset
  </div>
</section>

{#if showGameSettings && runtime}
  <GameSettingsDialog
    settings={runtime.settings}
    onclose={() => (showGameSettings = false)}
  />
{/if}

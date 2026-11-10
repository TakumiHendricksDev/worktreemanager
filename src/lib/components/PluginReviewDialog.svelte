<script lang="ts">
  /**
   * Add plugin from a folder or a Git repository, Review update, and the review every install goes
   * through, including a draft's.
   *
   * One dialog in two steps, choosing the source and then reading what it is, because the second
   * is the answer to the first. Moving to the review puts focus on its heading: the button that
   * had it is gone, and focus left on nothing falls to `<body>`, outside the dialog.
   *
   * # Cancelling
   *
   * A Git review can take minutes to clone, and the review is named by a token chosen here before
   * it starts so `pluginCancel` can stop it partway. Cancel therefore stays available while a
   * review is being prepared, and closing the dialog by any route cancels whatever it holds: a
   * preparation in flight or a finished review that was not installed. Only Install itself cannot
   * be abandoned once it has started.
   *
   * # Updates
   *
   * Review update starts from the tag, branch or commit the package was installed from as typed
   * (`requested`), not the commit it resolved to, which would review the same code again. A review
   * whose digest is the installed one has nothing to install, and Rust refuses it, so the dialog
   * says so instead of offering a button that can only fail.
   */
  import { onDestroy, untrack } from 'svelte';
  import { open } from '@tauri-apps/plugin-dialog';
  import { commands } from '../ipc/commands';
  import {
    packageParts,
    PACKAGE_PART_LABELS,
    shortRevision,
    versionLabel,
    type PluginReview,
    type PluginReviewStart,
    type PluginSourceKind,
  } from '../plugins';
  import { plugins, PluginTask } from '../state/plugins.svelte';
  import Banner from './ui/Banner.svelte';
  import Button from './ui/Button.svelte';
  import Dialog from './ui/Dialog.svelte';
  import Field from './ui/Field.svelte';
  import TextInput from './ui/TextInput.svelte';
  import PluginAccess from './PluginAccess.svelte';
  import PluginContributions from './PluginContributions.svelte';

  const {
    start,
    oncreate,
    onrefine,
    oninstalled,
    onclose,
  }: {
    start: PluginReviewStart;
    /** Switch to Create with an agent, which is another dialog. */
    oncreate: () => void;
    /** Leave the review and continue the draft's conversation. */
    onrefine: (draft: string) => void;
    oninstalled: (plugin: string, message: string) => void;
    onclose: () => void;
  } = $props();

  // The dialog is mounted per opening, so where it starts is read once.
  const initial = untrack(() => start);
  const update = initial.step === 'source' ? initial.update : null;
  const draft = initial.step === 'review' ? initial.draft : null;

  const task = new PluginTask();
  let step = $state<'source' | 'review'>(initial.step);
  let kind = $state<PluginSourceKind>(
    initial.step === 'source' ? initial.kind : initial.review.package.kind,
  );
  let source = $state(update?.current.source ?? '');
  let revision = $state(
    update ? (update.current.requested ?? update.current.revision) : 'main',
  );
  let review = $state<PluginReview | null>(
    initial.step === 'review' ? initial.review : null,
  );
  /** The token of a preparation still running, so Cancel can stop it. */
  let preparing = $state<string | null>(null);
  let heading = $state<HTMLElement | null>(null);
  let gone = false;

  const installedAs = $derived(
    review ? plugins.state.registry.installed[review.package.manifest.id] : update,
  );
  const updating = $derived(step === 'review' ? !!review?.replacing : update !== null);
  const unchanged = $derived(!!review && review.replacing === review.package.digest);
  const installing = $derived(step === 'review' && task.busy);
  const title = $derived.by(() => {
    if (updating) return 'Review update';
    return step === 'review' ? 'Review plugin' : 'Add plugin';
  });
  // `~` is accepted because `plugin_prepare` expands it, the same as the Create dialog's folder.
  const pathProblem = $derived(
    kind === 'local' && source.trim() !== '' && !/^(\/|~\/|~$)/.test(source.trim())
      ? 'Type the full path, starting with / or ~, or use Choose folder…'
      : null,
  );
  const parts = $derived(
    review ? packageParts(review.package.manifest).filter((p) => p.argv === null) : [],
  );

  $effect(() => {
    if (step === 'review') heading?.focus();
  });

  /** Let go of anything Rust is holding for this dialog. Safe to call twice. */
  function release() {
    gone = true;
    if (preparing) void commands.pluginCancel(preparing).catch(() => {});
    if (review) void commands.pluginCancel(review.token).catch(() => {});
    preparing = null;
    review = null;
  }
  onDestroy(release);

  function close() {
    if (installing) return;
    release();
    onclose();
  }

  async function browse() {
    await task.run(async () => {
      const path = await open({
        directory: true,
        multiple: false,
        title: 'Choose a plugin folder',
      });
      if (typeof path === 'string' && !gone) source = path;
    });
  }

  async function prepare() {
    if (task.busy || !source.trim() || pathProblem) return;
    const token = crypto.randomUUID();
    preparing = token;
    await task.run(async () => {
      const prepared = await commands.pluginPrepare(
        source.trim(),
        kind,
        kind === 'git' ? revision.trim() : '',
        token,
      );
      if (gone) {
        await commands.pluginCancel(prepared.token).catch(() => {});
        return;
      }
      review = prepared;
      step = 'review';
    });
    if (preparing === token) preparing = null;
  }

  async function install() {
    const pending = review;
    if (!pending || unchanged) return;
    const { id, name, version } = pending.package.manifest;
    if (await task.run(() => plugins.install(pending.token))) {
      // Installed, so there is nothing left for `release` to cancel.
      review = null;
      gone = true;
      oninstalled(
        id,
        pending.replacing
          ? `Updated ${name} to ${version}.`
          : `Installed ${name} ${version} and turned it on.`,
      );
    }
  }
</script>

<Dialog
  {title}
  onclose={close}
  closeDisabled={installing}
  onsubmit={(event) => {
    event.preventDefault();
    if (step === 'source') void prepare();
  }}
>
  {#snippet body()}
    <div class="o-stack">
      {#if step === 'source'}
        {#if update}
          <p>
            {update.current.kind === 'git'
              ? `Updating ${update.current.manifest.name} from ${versionLabel(update.current)}. Choose the tag, branch or commit to review; it was installed from ${update.current.requested ?? shortRevision(update.current.revision)}.`
              : `Reloading ${update.current.manifest.name} ${update.current.manifest.version} from its folder. Nothing changes until you review and install the new snapshot.`}
          </p>
        {:else}
          <div class="o-row" role="group" aria-label="How to add a plugin">
            <Button size="sm" variant="quiet" ariaPressed={false} onclick={oncreate}
              >Create with an agent</Button
            >
            <Button
              size="sm"
              variant={kind === 'local' ? 'neutral' : 'quiet'}
              ariaPressed={kind === 'local'}
              disabled={task.busy}
              onclick={() => {
                kind = 'local';
              }}>Local folder</Button
            >
            <Button
              size="sm"
              variant={kind === 'git' ? 'neutral' : 'quiet'}
              ariaPressed={kind === 'git'}
              disabled={task.busy}
              onclick={() => {
                kind = 'git';
              }}>Git repository</Button
            >
          </div>
        {/if}
        <Field
          id="plugin-source"
          label={kind === 'git' ? 'Repository URL' : 'Plugin folder'}
          help={kind === 'git'
            ? 'An HTTPS address, without a user name or password in it.'
            : 'The folder that holds wtm-plugin.toml, as a full path. ~ means your home folder.'}
          errors={[pathProblem]}
        >
          {#if kind === 'git'}
            <TextInput
              id="plugin-source"
              bind:value={source}
              mono
              disabled={task.busy}
              placeholder="https://github.com/owner/plugin"
            />
          {:else}
            <div class="o-row c-plugins__inline">
              <TextInput
                id="plugin-source"
                bind:value={source}
                mono
                disabled={task.busy}
                placeholder="/Users/you/Plugins/my-plugin"
              /><Button disabled={task.busy} onclick={() => void browse()}
                >Choose folder…</Button
              >
            </div>
          {/if}
        </Field>
        {#if kind === 'git'}<Field
            id="plugin-revision"
            label="Tag, branch or commit"
            help="Review resolves this to an exact commit. Nothing updates automatically."
            ><TextInput
              id="plugin-revision"
              bind:value={revision}
              mono
              disabled={task.busy}
            /></Field
          >{/if}
        <p class="c-note">Nothing in the plugin runs before you review it.</p>
      {:else if review}
        <h3 class="c-pane-title" tabindex="-1" bind:this={heading}>
          {review.package.manifest.name} · {review.package.manifest.version}
        </h3>
        <p>{review.package.manifest.description}</p>
        {#if draft}<p class="c-note">
            Created with {draft.provider === 'claude' ? 'Claude' : 'Codex'}. Its manifest
            and files pass validation.
          </p>{/if}
        <dl class="o-facts">
          <dt>Source</dt>
          <dd>{review.package.source}</dd>
          <dt>Revision</dt>
          <dd>
            {review.package
              .revision}{#if review.package.requested && review.package.requested !== review.package.revision}{` (from ${review.package.requested})`}{/if}
          </dd>
          <dt>Digest</dt>
          <dd class="u-mono">{review.package.digest}</dd>
        </dl>
        {#if review.replacing && installedAs}
          <p>
            Updating {review.package.manifest.name} from {versionLabel(installedAs.current)} to
            {versionLabel(review.package)}.
          </p>
        {/if}
        {#if unchanged}
          <Banner variant="info"
            >No changes since the installed version, so there is nothing to install.</Banner
          >
        {/if}
        <PluginAccess
          manifest={review.package.manifest}
          executable={review.executable}
          level={4}
        />
        {#if parts.length}
          <h4 class="c-section-heading">Adds</h4>
          <ul class="o-plain-list o-stack">
            {#each parts as part (part.kind + part.id)}
              <li>
                <strong>{PACKAGE_PART_LABELS[part.kind]}</strong> · {part.title}{#if part.detail}<span
                    class="c-note">{` · ${part.detail}`}</span
                  >{/if}
              </li>
            {/each}
          </ul>
        {/if}
        <PluginContributions review={review.package.manifest.extensions} level={4} />
        {#if draft}<p class="c-note">
            Installing saves this reviewed snapshot; later edits need a new review.
            Validation checks the packaging, not whether the plugin behaves correctly. Your
            region choices stay as they are.
          </p>{/if}
      {/if}
      {#if task.error}<Banner>{task.error}</Banner>{/if}
    </div>
  {/snippet}
  {#snippet footer()}
    <Button disabled={installing} onclick={close}
      >{step === 'review' && draft ? 'Keep draft' : 'Cancel'}</Button
    >
    {#if step === 'review' && draft}<Button
        disabled={installing}
        onclick={() => {
          const id = draft.id;
          close();
          onrefine(id);
        }}>Refine with agent…</Button
      >{/if}
    {#if step === 'source'}<Button
        type="submit"
        variant="accent"
        disabled={task.busy || !source.trim() || !!pathProblem}
        >{task.busy ? (kind === 'git' ? 'Fetching…' : 'Reading…') : 'Review plugin'}</Button
      >
    {:else}<Button
        variant="accent"
        disabled={task.busy || unchanged}
        onclick={() => void install()}
        >{installing ? 'Installing…' : 'Install & enable'}</Button
      >{/if}
  {/snippet}
</Dialog>

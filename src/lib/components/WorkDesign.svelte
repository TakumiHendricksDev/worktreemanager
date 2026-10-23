<script lang="ts">
  /**
   * An issue's Design tab: the designer's revisions, your comments on each, your decision, and the
   * gate in front of implementing.
   *
   * # In the order you act on it
   *
   * 1. **The gate**, first: whether implementing may start, and if not, the one thing in the way.
   *    It is Rust's word (`implementing_gate`), and Start implementing asks Rust again; a click that
   *    the gate refuses says why and changes nothing.
   * 2. **Revisions**, newest first, every one of them. Each opens in the existing sandboxed review
   *    pane, in the issue's worktree, keeps its own comments, and shows its fingerprint, the brief
   *    it was drawn from, the feedback it answered, its reuse notes and the files it drew on.
   * 3. **The decision**, on the newest revision only: Approve, or Request changes, which sends your
   *    note and that revision's comments to the designer in the same conversation. A newer revision
   *    replaces an approval of an older one, and an edit to a file it was drawn from makes one stale.
   *
   * # What you approve is what you saw
   *
   * Approve sends the revision's number and fingerprint as this tab shows them. Rust reads the
   * artifact back and the sources from the worktree, and refuses when either moved.
   */
  import { untrack } from 'svelte';

  import {
    BLOCKED_WORDS,
    designRunWords,
    designSessionWords,
    designStandingWords,
    revisionMark,
    shortFingerprint,
  } from '../design';
  import { filedAt as formatTime } from '../inbox';
  import { conversation } from '../plan';
  import { design } from '../state/design.svelte';
  import CodeText from './CodeText.svelte';
  import Markdown from './Markdown.svelte';
  import Button from './ui/Button.svelte';

  const {
    projectId,
    issueKey,
    visible,
    onopenworktree,
  }: {
    projectId: string;
    issueKey: string;
    visible: boolean;
    /** Show the issue's worktree, where the review pane opened. */
    onopenworktree: (worktreeId: string) => void;
  } = $props();

  // Read when shown, untracked: `load` writes the store it would otherwise be tracking.
  $effect(() => {
    const project = projectId;
    const key = issueKey;
    if (visible) untrack(() => void design.load(project, key));
  });

  const view = $derived(design.views[issueKey] ?? null);
  const busy = $derived(design.busy[issueKey] ?? false);
  const error = $derived(design.errors[issueKey] ?? null);
  const newestFirst = $derived(view ? [...view.revisions].reverse() : []);
  const latest = $derived(view?.revisions.at(-1) ?? null);

  let requesting = $state(false);
  let request = $state('');
  let changing = $state(false);
  let note = $state('');
  /** Comment drafts, by revision. */
  let drafts = $state<Record<number, string>>({});

  async function requestDesign(): Promise<void> {
    if (!view) return;
    if (await design.request(projectId, issueKey, view.flow.id, request)) {
      request = '';
      requesting = false;
    }
  }

  async function comment(revision: number): Promise<void> {
    const text = drafts[revision]?.trim() ?? '';
    if (!view || text === '') return;
    if (await design.comment(projectId, issueKey, view.flow.id, revision, text)) {
      drafts = { ...drafts, [revision]: '' };
    }
  }

  async function requestChanges(): Promise<void> {
    if (!view || !latest) return;
    const ok = await design.requestChanges(
      projectId,
      issueKey,
      view.flow.id,
      latest.revision.revision,
      note,
    );
    if (ok) {
      note = '';
      changing = false;
    }
  }

  async function preview(revision: number): Promise<void> {
    if (!view?.design) return;
    const worktree = view.design.worktree;
    const opened = await design.preview(projectId, issueKey, view.flow.id, revision);
    if (opened) onopenworktree(worktree);
  }

  const commentsOn = (revision: number) =>
    view?.comments.filter((c) => c.revision === revision) ?? [];
  const unsent = $derived(latest ? commentsOn(latest.revision.revision).length : 0);
</script>

<div class="c-design">
  {#if error}
    <p class="c-design__error c-status--danger" role="alert"><CodeText text={error} /></p>
  {/if}

  {#if !view}
    <div class="c-placeholder">
      <p>
        {issueKey} has no run yet. Start work gives it a worktree and a plan; a design follows
        an approved plan that needs one, or your request.
      </p>
    </div>
  {:else}
    <header class="c-design__head">
      <h2 class="c-design__title">{designRunWords(view)}</h2>
      <span class="c-design__actions">
        {#if view.canRequest}
          <Button
            size="sm"
            disabled={busy}
            ariaExpanded={requesting}
            title="Have the designer draw this issue's screens from the approved plan before anything is built"
            onclick={() => (requesting = !requesting)}>Request a design</Button
          >
        {/if}
        {#if view.canStart}
          <Button
            variant="accent"
            size="sm"
            disabled={busy}
            title="Run the designer in the issue's worktree, read-only, in its own conversation"
            onclick={() => void design.start(projectId, issueKey, view.flow.id)}
            >{view.sessions.length === 0 ? 'Start designer' : 'Resume designer'}</Button
          >
        {/if}
        {#if view.canImplement}
          <Button
            variant={view.gate.ready ? 'accent' : 'neutral'}
            size="sm"
            disabled={busy}
            title={view.gate.ready
              ? 'Move the run to Implementing: the plan and the design it needs are approved'
              : `Implementing waits: ${view.gate.reason ?? ''}`}
            onclick={() => void design.implement(projectId, issueKey, view.flow.id)}
            >Start implementing</Button
          >
        {/if}
      </span>
    </header>

    <p
      class="c-design__gate c-status--{view.gate.ready ? 'ok' : 'warn'}"
      data-gate={view.gate.ready ? 'ready' : (view.gate.blocked?.kind ?? 'blocked')}
    >
      {#if view.flow.state.phase === 'implementing'}
        Implementing. The plan{view.required ? ' and the design' : ''} it needed were approved;
        the implementer arrives with a later version of wtm, and nothing writes yet.
      {:else if view.gate.ready}
        Implementing may start: the plan is approved{view.required
          ? `, and so is design revision ${view.standing.kind === 'approved' ? view.standing.revision : ''}`
          : view.skipDesign
            ? ', with the design skipped'
            : ', and it needs no design'}.
      {:else if view.gate.blocked}
        Implementing waits: {BLOCKED_WORDS[view.gate.blocked.kind]}.
      {/if}
    </p>

    {#if requesting}
      <form
        class="c-design__form"
        onsubmit={(event) => {
          event.preventDefault();
          void requestDesign();
        }}
      >
        <label class="c-design__field">
          What should the design show? (optional)
          <textarea class="c-textarea" rows="3" bind:value={request}></textarea>
        </label>
        <div class="c-design__row">
          <Button variant="accent" type="submit" disabled={busy}>Start the design</Button>
          <Button variant="quiet" onclick={() => (requesting = false)}>Cancel</Button>
        </div>
        <p class="c-design__note">
          Once a design is started, implementing waits for a revision you approve.
        </p>
      </form>
    {/if}

    {#if view.designer === null && (view.flow.state.phase === 'designing' || view.designNeeded)}
      <p class="c-design__note">
        This project names no designer. Add
        <span class="u-mono">designer = "&lt;role&gt;"</span> to its
        <span class="u-mono">[workflow]</span> in your own config, naming a reviewer role that
        reads the worktree.
      </p>
    {:else if view.running}
      <p class="c-design__note">
        The designer is drawing
        {latest ? `revision ${latest.revision.revision + 1}` : 'the first revision'},
        read-only, in the issue’s worktree, as role
        <span class="u-mono">{view.design?.role ?? 'designer'}</span>. Its prototype is kept
        under wtm’s data directory, never in the source.
      </p>
    {/if}

    {#if view.revisions.length > 0}
      <p
        class="c-design__standing c-status--{view.standing.kind === 'approved'
          ? 'ok'
          : view.standing.kind === 'stale'
            ? 'warn'
            : 'muted'}"
      >
        {designStandingWords(view.standing)}
      </p>
    {/if}

    {#each newestFirst as shown (shown.revision.id)}
      {@const revision = shown.revision}
      {@const newest = revision.revision === latest?.revision.revision}
      {@const mark = revisionMark(view, revision.revision)}
      <section
        class="c-design-revision"
        class:is-current={newest}
        aria-labelledby="design-revision-{revision.revision}"
      >
        <div class="c-design-revision__meta">
          <span class="c-design-revision__badge" id="design-revision-{revision.revision}"
            >Design · revision {revision.revision}</span
          >
          {#if mark}
            <span class="c-design-revision__mark c-design-revision__mark--{mark.tone}"
              >{mark.words}</span
            >
          {/if}
          <span>published {formatTime(revision.submittedAt)}</span>
          <span title={revision.fingerprint}
            >fingerprint <span class="u-mono">{shortFingerprint(revision.fingerprint)}</span
            ></span
          >
          <span>for plan revision {revision.planRevision}</span>
        </div>
        <p class="c-design-revision__brief"><CodeText text={revision.brief} /></p>
        {#if revision.feedback}
          <div class="c-design-revision__part">
            <span class="c-design__label">Answers</span>
            <p class="c-design-revision__text"><CodeText text={revision.feedback} /></p>
          </div>
        {/if}
        {#if !shown.readable}
          <p class="c-design__warn c-status--warn" role="note">
            Its artifact no longer reads as it was published.
          </p>
        {/if}
        {#if shown.changedSources.length > 0}
          <p class="c-design__warn c-status--warn" role="note">
            Changed since it was drawn:
            {#each shown.changedSources as path, index (path)}
              <span class="u-mono">{path}</span>{index < shown.changedSources.length - 1
                ? ', '
                : ''}
            {/each}
          </p>
        {/if}
        <details class="c-design-revision__notes" open={newest}>
          <summary>Reuse notes and sources</summary>
          <div class="c-design-revision__reuse"><Markdown source={revision.reuse} /></div>
          {#if revision.sources.length > 0}
            <span class="c-design__label">Drawn from</span>
            <ul class="o-plain-list c-design-revision__sources">
              {#each revision.sources as source (source.path)}
                <li><span class="u-mono">{source.path}</span></li>
              {/each}
            </ul>
          {/if}
        </details>

        <div class="c-design__row">
          <Button
            size="sm"
            disabled={busy || !shown.readable}
            title="Open this revision in the sandboxed review pane, in the issue's worktree"
            onclick={() => void preview(revision.revision)}>Preview</Button
          >
          {#if newest && view.canDecide}
            <Button
              variant="accent"
              size="sm"
              disabled={busy ||
                view.standing.kind === 'approved' ||
                !shown.readable ||
                shown.changedSources.length > 0}
              title={!shown.readable || shown.changedSources.length > 0
                ? 'What it was drawn from, or the revision itself, changed: ask for a fresh revision'
                : 'Approve this revision, as published, for implementing'}
              onclick={() =>
                void design.approve(
                  projectId,
                  issueKey,
                  view.flow.id,
                  revision.revision,
                  revision.fingerprint,
                )}
              >{view.standing.kind === 'approved'
                ? `Revision ${revision.revision} approved`
                : `Approve revision ${revision.revision}`}</Button
            >
            <Button
              size="sm"
              disabled={busy}
              ariaExpanded={changing}
              onclick={() => (changing = !changing)}>Request changes</Button
            >
          {/if}
        </div>

        {#if newest && changing && view.canDecide}
          <form
            class="c-design__form"
            onsubmit={(event) => {
              event.preventDefault();
              void requestChanges();
            }}
          >
            <label class="c-design__field">
              What should change?
              <textarea class="c-textarea" rows="3" bind:value={note}></textarea>
            </label>
            <div class="c-design__row">
              <Button
                variant="accent"
                type="submit"
                disabled={busy || (note.trim() === '' && unsent === 0)}
                >Send to the designer</Button
              >
              <Button variant="quiet" onclick={() => (changing = false)}>Cancel</Button>
            </div>
            <p class="c-design__note">
              The designer gets your note{unsent > 0
                ? ` and the ${unsent} comment${unsent === 1 ? '' : 's'} on this revision`
                : ''} in the same conversation, and publishes the next revision. That revision
              replaces any approval of this one.
            </p>
          </form>
        {/if}

        <div class="c-design-revision__comments">
          {#each commentsOn(revision.revision) as said (said.id)}
            <p class="c-design-revision__comment">
              <CodeText text={said.text} />
              <span class="c-design-revision__when">{formatTime(said.at)}</span>
            </p>
          {/each}
          <form
            class="c-design-revision__add"
            onsubmit={(event) => {
              event.preventDefault();
              void comment(revision.revision);
            }}
          >
            <label class="u-visually-hidden" for="design-comment-{revision.revision}"
              >Comment on revision {revision.revision}</label
            >
            <input
              id="design-comment-{revision.revision}"
              class="c-input"
              placeholder="Comment on revision {revision.revision}, e.g. Header: use PageHeader"
              value={drafts[revision.revision] ?? ''}
              oninput={(event) =>
                (drafts = {
                  ...drafts,
                  [revision.revision]: (event.currentTarget as HTMLInputElement).value,
                })}
            />
            <Button
              size="sm"
              type="submit"
              disabled={busy || (drafts[revision.revision] ?? '').trim() === ''}
              >Comment</Button
            >
          </form>
        </div>
      </section>
    {/each}

    {#if view.decisions.length > 0}
      <details class="c-design-history">
        <summary>Decisions</summary>
        <ul class="c-design-history__list">
          {#each view.decisions as decision (decision.id)}
            <li>
              {formatTime(decision.at)} · revision {decision.revision}:
              {decision.verdict.kind === 'approved'
                ? 'approved'
                : `changes asked for — “${decision.verdict.note}”`}
            </li>
          {/each}
        </ul>
      </details>
    {/if}

    {#if view.sessions.length > 0}
      <details class="c-design-history">
        <summary>Designer sessions · one conversation</summary>
        <ul class="o-plain-list c-design-history__list">
          {#each view.sessions as session (session.id)}
            <li>
              {designSessionWords(session)} · conversation
              <span class="u-mono" title={session.providerSession ?? ''}
                >{conversation(session.providerSession)}</span
              >
            </li>
          {/each}
        </ul>
      </details>
    {/if}
  {/if}
</div>

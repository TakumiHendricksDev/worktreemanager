<script lang="ts">
  /**
   * One consolidated finding in the Reviews tab: what it is, where, who reported it and how they
   * know, and the three things you can do with it.
   *
   * # Every report is shown, not just the one that leads
   *
   * A finding merged from two roles says so: each role is named under it, and the reports beyond
   * the first are behind a disclosure with their own titles and bodies, because a second model
   * describing the same lines differently is information the merge should not throw away.
   *
   * # Dismissing asks why
   *
   * The reason is what a later review reads and what stops the same finding being argued twice,
   * so Dismiss opens a one-line form rather than acting on the click. A dismissed finding is listed
   * under Dismissed with its reason, and Restore takes the dismissal back.
   */
  import type { FindingRecord, ReviewRunView } from '../ipc/types';
  import { SEVERITY_WORDS } from '../knowledge';
  import { classLine, findingPlace, findingSources } from '../review-run';
  import CodeText from './CodeText.svelte';
  import Button from './ui/Button.svelte';
  import TextInput from './ui/TextInput.svelte';

  const {
    finding,
    view,
    stale,
    busy,
    onopen,
    onsaverule,
    ondismiss,
  }: {
    finding: FindingRecord;
    view: ReviewRunView;
    /** Every report of it was on a file that changed since. */
    stale: boolean;
    busy: boolean;
    onopen: (path: string, line: number | null) => void;
    onsaverule: (finding: FindingRecord) => void;
    ondismiss: (finding: FindingRecord, reason: string | null) => void;
  } = $props();

  const place = $derived(findingPlace(finding));
  const sources = $derived(findingSources(finding));
  const evidenceLine = $derived(classLine(finding, view));
  /** Reports beyond the one whose words the finding takes. */
  const others = $derived(finding.evidence.slice(1));
  /** For a rule violation, whether the rule it breaks is required, as the role was told. */
  const ruleKind = $derived.by(() => {
    const cls = finding.class;
    if (cls.kind !== 'rule_violation') return null;
    return (
      view.applied.rules.find((r) => r.rule === cls.rule && r.revision === cls.revision)
        ?.kind ?? null
    );
  });
  const checkRefs = $derived(
    finding.evidence
      .flatMap((e) => e.reported.evidence ?? [])
      .filter((line) => line.trim()),
  );

  let dismissing = $state(false);
  let reason = $state('');

  function dismiss(event: SubmitEvent): void {
    event.preventDefault();
    if (reason.trim() === '') return;
    ondismiss(finding, reason.trim());
    dismissing = false;
    reason = '';
  }
</script>

<article
  class="c-finding"
  class:is-dismissed={finding.dismissed !== null}
  aria-labelledby="finding-{finding.id}"
>
  <div class="c-finding__meta">
    <span class="c-finding__severity c-finding__severity--{finding.severity}">
      {ruleKind === 'required' ? 'Required rule' : SEVERITY_WORDS[finding.severity]}
    </span>
    {#if stale}
      <span
        class="c-finding__stale"
        title="Every report of it is on a file that changed since">Stale</span
      >
    {/if}
    {#if place && finding.location}
      <Button
        variant="link"
        size="sm"
        title="Open in the Code tab"
        onclick={() =>
          finding.location &&
          onopen(finding.location.path, finding.lines ? finding.location.startLine : null)}
        ><span class="u-mono">{place}</span></Button
      >
    {:else}
      <span class="c-finding__where">About the change as a whole</span>
    {/if}
    <span class="c-finding__sources">{sources.join(' · ')}</span>
  </div>

  <h4 class="c-finding__title" id="finding-{finding.id}">
    <CodeText text={finding.title} />
  </h4>
  <p class="c-finding__body"><CodeText text={finding.rationale} /></p>
  {#if finding.requirement}
    <p class="c-finding__note">Requirement: <CodeText text={finding.requirement} /></p>
  {/if}
  {#if evidenceLine}
    <p
      class="c-finding__note"
      class:c-finding__note--verified={finding.class.kind === 'confirmed_bug'}
    >
      <CodeText text={evidenceLine} />
    </p>
  {/if}
  {#if checkRefs.length > 0}
    <ul class="c-finding__refs" aria-label="Evidence">
      {#each checkRefs as line, index (index)}
        <li><CodeText text={line} /></li>
      {/each}
    </ul>
  {/if}

  {#if others.length > 0}
    <details class="c-finding__reports">
      <summary
        >Also reported by {others.length === 1
          ? 'one more report'
          : `${others.length} more reports`}</summary
      >
      <ul class="c-finding__refs">
        {#each others as other (`${other.source.invocation}:${other.source.ordinal}`)}
          <li>
            <strong>{other.source.role}</strong>:
            <CodeText text={other.reported.title} /> — <CodeText
              text={other.reported.body}
            />
            {#if other.reported.verification}
              (verified: <CodeText text={other.reported.verification} />)
            {/if}
          </li>
        {/each}
      </ul>
    </details>
  {/if}

  {#if finding.dismissed}
    <p class="c-finding__note">
      Dismissed: <CodeText text={finding.dismissed.reason} />
    </p>
    <div class="c-finding__actions">
      <Button
        size="sm"
        variant="quiet"
        disabled={busy}
        onclick={() => ondismiss(finding, null)}>Restore</Button
      >
    </div>
  {:else if dismissing}
    <form class="c-finding__dismiss" onsubmit={dismiss}>
      <TextInput
        bind:value={reason}
        placeholder="Why it does not need fixing"
        ariaLabel="Why this finding is dismissed"
      />
      <Button type="submit" size="sm" disabled={busy || reason.trim() === ''}
        >Dismiss</Button
      >
      <Button size="sm" variant="quiet" onclick={() => (dismissing = false)}>Cancel</Button>
    </form>
  {:else}
    <div class="c-finding__actions">
      {#if finding.location}
        <Button
          size="sm"
          onclick={() =>
            finding.location &&
            onopen(
              finding.location.path,
              finding.lines ? finding.location.startLine : null,
            )}>Open in Code</Button
        >
      {/if}
      <Button size="sm" onclick={() => onsaverule(finding)}>Save as rule…</Button>
      <Button size="sm" variant="quiet" onclick={() => (dismissing = true)}>Dismiss…</Button
      >
    </div>
  {/if}
</article>

<script lang="ts" module>
  import type { Finding, RuleView } from '../knowledge';

  /**
   * What the dialog is for. One editor serves all five because they edit the same draft; what
   * differs is the title, the line under it, the button, and which command saves.
   */
  export type RuleDialogMode =
    | { kind: 'new' }
    /** Save as rule, from a review finding. `subject` is its work item, such as `APP-8701`. */
    | { kind: 'finding'; finding: Finding; subject: string | null }
    | { kind: 'edit'; rule: RuleView }
    /** Edit first, for an agent's proposal: saving approves it as you wrote it. */
    | { kind: 'review'; rule: RuleView }
    | { kind: 'replace'; rule: RuleView };
</script>

<script lang="ts">
  /**
   * The rule editor: Save as rule, New rule, Edit, Edit first and Replace.
   *
   * # Who the rule belongs to is said, not implied
   *
   * The note at the foot of the form says whether saving makes the rule apply, because that is the
   * one thing that differs by who wrote it and it is invisible otherwise: yours applies on save, an
   * agent's waits for you, and a dismissed finding never offers this dialog at all.
   *
   * # The conflict check is shown, and off
   *
   * The approved design checks a new rule against the repository's instructions before saving, with
   * a short read-only review pass. That pass needs review runs, which this build does not have, so
   * the box is drawn disabled and says so rather than being left out: leaving it out would read as a
   * decision not to check, which nobody made.
   */
  import { commands } from '../ipc/commands';
  import { errorMessage, isWtmError } from '../ipc/types';
  import {
    KIND_WORDS,
    SEVERITY_WORDS,
    current,
    draftFromFinding,
    emptyDraft,
    locationText,
    ruleLabel,
    splitLines,
    splitList,
    type RuleDraft,
    type RuleKind,
  } from '../knowledge';
  import Banner from './ui/Banner.svelte';
  import Button from './ui/Button.svelte';
  import Choice from './ui/Choice.svelte';
  import Dialog from './ui/Dialog.svelte';
  import Field from './ui/Field.svelte';
  import TextInput from './ui/TextInput.svelte';

  const {
    projectId,
    projectName,
    mode,
    onclose,
    onsaved,
  }: {
    projectId: string;
    projectName: string;
    mode: RuleDialogMode;
    onclose: () => void;
    onsaved: (rule: RuleView) => void;
  } = $props();

  // Read once: the dialog edits a copy, and a list refresh behind it must not reset the form.
  // svelte-ignore state_referenced_locally
  const start: RuleDraft = startingDraft(mode);

  function startingDraft(m: RuleDialogMode): RuleDraft {
    switch (m.kind) {
      case 'new':
        return emptyDraft();
      case 'finding':
        return draftFromFinding(m.finding) ?? emptyDraft();
      default:
        return $state.snapshot(current(m.rule).draft);
    }
  }

  let text = $state(start.text);
  let rationale = $state(start.rationale);
  let kind = $state<RuleKind>(start.kind);
  let areas = $state(start.scope.areas.join(', '));
  let paths = $state(start.scope.paths.join(', '));
  let prefer = $state(
    start.examples
      .filter((e) => e.stance === 'prefer')
      .map((e) => e.text)
      .join('\n'),
  );
  let avoid = $state(
    start.examples
      .filter((e) => e.stance === 'avoid')
      .map((e) => e.text)
      .join('\n'),
  );
  let references = $state(start.references.join('\n'));

  let busy = $state(false);
  let error = $state<string | null>(null);
  /** A glob the backend refused, shown under Files rather than in the banner. */
  let globError = $state<string | null>(null);

  const title = $derived.by(() => {
    switch (mode.kind) {
      case 'new':
        return 'New review rule';
      case 'finding':
        return 'Save as review rule';
      case 'edit':
        return `Edit ${ruleLabel(mode.rule.id)}`;
      case 'review':
        return `Edit and approve ${ruleLabel(mode.rule.id)}`;
      case 'replace':
        return `Replace ${ruleLabel(mode.rule.id)}`;
    }
  });

  const submitLabel = $derived.by(() => {
    switch (mode.kind) {
      case 'edit':
        return `Save version ${current(mode.rule).number + 1}`;
      case 'review':
        return 'Save and approve';
      case 'replace':
        return 'Replace rule';
      default:
        return 'Save rule';
    }
  });

  const note = $derived.by(() => {
    switch (mode.kind) {
      case 'review':
        return 'An agent proposed this rule, so it has never governed a review. Saving approves it exactly as you leave it here.';
      case 'edit':
        return `Reviews from now on read version ${current(mode.rule).number + 1}. Findings that cited an earlier version still read that one.`;
      case 'replace':
        return `The new rule takes ${ruleLabel(mode.rule.id)}'s place and is active as soon as it's saved. ${ruleLabel(mode.rule.id)} stays readable, marked as replaced.`;
      default:
        return `You wrote this rule, so it is active as soon as it's saved. It stays private to ${projectName}. Dismissing a finding never creates a rule, and agent-proposed rules wait for your approval.`;
    }
  });

  function draft(): RuleDraft {
    return {
      kind,
      text,
      rationale,
      scope: { areas: splitList(areas), paths: splitList(paths) },
      examples: [
        ...splitLines(prefer).map((t) => ({ stance: 'prefer' as const, text: t })),
        ...splitLines(avoid).map((t) => ({ stance: 'avoid' as const, text: t })),
      ],
      references: splitLines(references),
    };
  }

  async function save(event: Event) {
    event.preventDefault();
    if (busy) return;
    error = null;
    globError = null;
    if (text.trim() === '') {
      error = 'A rule needs some text.';
      return;
    }
    busy = true;
    try {
      const d = draft();
      let saved: RuleView;
      switch (mode.kind) {
        case 'new':
          saved = await commands.createRule(projectId, d);
          break;
        case 'finding':
          saved = await commands.saveFindingAsRule(projectId, mode.finding, d);
          break;
        case 'edit':
          saved = await commands.changeRule(projectId, mode.rule.id, {
            kind: 'revise',
            draft: d,
          });
          break;
        case 'review':
          saved = await commands.changeRule(projectId, mode.rule.id, {
            kind: 'approve_revised',
            draft: d,
          });
          break;
        case 'replace':
          saved = await commands.replaceRule(projectId, mode.rule.id, d);
          break;
      }
      onsaved(saved);
    } catch (e) {
      const detail = isWtmError(e) ? (e.detail as { kind?: string } | null) : null;
      if (detail?.kind === 'bad_glob') globError = errorMessage(e);
      else error = errorMessage(e);
    } finally {
      busy = false;
    }
  }
</script>

<Dialog {title} {onclose} onsubmit={save} closeDisabled={busy} wide>
  {#snippet body()}
    <div class="o-stack c-rule-form">
      {#if mode.kind === 'finding'}
        <p class="c-rule-form__from">
          From finding
          <span class="c-rule-form__severity c-rule-form__severity--{mode.finding.severity}"
            >{SEVERITY_WORDS[mode.finding.severity]}</span
          >
          {#if mode.finding.location}<code>{locationText(mode.finding.location)}</code>{/if}
          {#if mode.subject}on {mode.subject}{/if}
        </p>
      {:else if mode.kind === 'review' && mode.rule.source.kind === 'proposal'}
        <p class="c-rule-form__from">
          Proposed by the {mode.rule.source.role} role{mode.rule.sourceSubject
            ? ` on ${mode.rule.sourceSubject}`
            : ''}
        </p>
      {/if}

      {#if error}<Banner>{error}</Banner>{/if}

      <Field id="rule-text" label="Rule">
        <textarea
          id="rule-text"
          class="c-textarea"
          rows="2"
          bind:value={text}
          placeholder="Use AppDialog for confirm and cancel flows"></textarea>
      </Field>
      <Field id="rule-why" label="Why">
        <textarea
          id="rule-why"
          class="c-textarea"
          rows="2"
          bind:value={rationale}
          placeholder="What goes wrong without it, so a reviewer can judge the edge cases"
        ></textarea>
      </Field>

      <fieldset class="c-rule-form__kinds">
        <legend class="c-rule-form__legend">Kind</legend>
        {#each ['guidance', 'required'] as const as option (option)}
          <Choice
            type="radio"
            name="rule-kind"
            checked={kind === option}
            onchange={() => (kind = option)}
          >
            <strong class="c-rule-form__kind">{KIND_WORDS[option].label}</strong>
            <span class="c-rule-form__kind-help">{KIND_WORDS[option].help}</span>
          </Choice>
        {/each}
      </fieldset>

      <div class="c-rule-form__grid">
        <Field id="rule-repository" label="Repository">
          <TextInput id="rule-repository" value={projectName} disabled />
        </Field>
        <Field
          id="rule-areas"
          label="Areas"
          help="Comma-separated, such as frontend, exports."
        >
          <TextInput id="rule-areas" bind:value={areas} placeholder="frontend" />
        </Field>
        <Field
          id="rule-paths"
          label="Files"
          help="Globs from the repository root. Empty means anywhere."
          errors={[globError]}
        >
          <TextInput id="rule-paths" bind:value={paths} mono placeholder="src/**/*.vue" />
        </Field>
        <Field id="rule-references" label="References" help="One per line.">
          <textarea
            id="rule-references"
            class="c-textarea"
            rows="1"
            bind:value={references}
            placeholder="docs/ui/dialogs.md"></textarea>
        </Field>
        <Field id="rule-prefer" label="Preferred example" help="One per line.">
          <textarea
            id="rule-prefer"
            class="c-textarea c-rule-form__mono"
            rows="1"
            bind:value={prefer}
            placeholder="src/components/AppDialog.vue"></textarea>
        </Field>
        <Field id="rule-avoid" label="Example to avoid" help="One per line.">
          <textarea
            id="rule-avoid"
            class="c-textarea c-rule-form__mono"
            rows="1"
            bind:value={avoid}
            placeholder="src/components/BaseModal.vue"></textarea>
        </Field>
      </div>

      <Choice disabled>
        Check it against the repository's instructions first
        <span class="c-badge">later</span>
      </Choice>
      <p class="c-field__help c-rule-form__later">
        A short read-only pass that reports a conflict before saving. It needs review runs,
        which this build does not have yet.
      </p>

      <p class="c-rule-form__note">{note}</p>
    </div>
  {/snippet}
  {#snippet footer()}
    <Button onclick={onclose} disabled={busy}>Cancel</Button>
    <Button variant="accent" type="submit" disabled={busy}>
      {busy ? 'Saving…' : submitLabel}
    </Button>
  {/snippet}
</Dialog>

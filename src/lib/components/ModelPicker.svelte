<script lang="ts">
  /**
   * One control for a model and its effort, and the reason it is one control.
   *
   * **The effort ladder belongs to the model, not the provider.** Codex reports six efforts for
   * `gpt-5.6-sol` — including `ultra` — and four for `gpt-5.5`. A single effort list would therefore
   * offer rungs the selected model rejects, so changing the model changes the ladder, and picking a
   * model whose ladder lacks the current effort snaps to that model's own default.
   *
   * # Why native selects, worn under a drawn label
   *
   * Native, for the same reason `OpenInButton` is: the popup renders outside the stacking context, so
   * neither menu needs a `z-index` — and `settings/_config.scss` says the app has two stacking levels
   * and nothing else should add one. A hand-built listbox would have needed a third.
   *
   * Drawn label, because a `<select>` is as wide as its **widest option** rather than its selected
   * one. As plain form controls these two were sized by the longest model id in the list and had to
   * be capped and ellipsized to fit a narrow pane — the cap then truncating the *selected* model,
   * which is the one thing the control exists to show. `o-overlay-select` is the app's existing
   * answer; `_input.scss` draws the line these two were on the wrong side of.
   *
   * # `ultracode` is a checkbox, not a sixth rung
   *
   * However much the name suggests one. It is a boolean meaning "xhigh plus standing workflow
   * orchestration", and the CLI refuses it if effort resolves below xhigh — so the checkbox disables
   * itself below that and says why. Codex's `ultra` *is* a rung and appears in the effort list.
   * Two similar names, two different things.
   */
  import type { Capability } from '../ipc/types';
  import Choice from './ui/Choice.svelte';
  import Icon from './ui/Icon.svelte';

  const {
    capability,
    model,
    effort,
    flags,
    disabled = false,
    onchange,
  }: {
    capability: Capability | null;
    model: string | null;
    effort: string | null;
    /** Provider flags that are on. Keys match `capability.flags`. */
    flags: string[];
    disabled?: boolean;
    onchange: (next: { model: string; effort: string; flags: string[] }) => void;
  } = $props();

  const models = $derived(capability?.models ?? []);
  const selected = $derived(
    models.find((m) => m.id === model) ??
      models.find((m) => m.isDefault) ??
      models[0] ??
      null,
  );
  /** The ladder for the selected model, which is the whole point of this component. */
  const efforts = $derived(selected?.efforts ?? []);
  const currentEffort = $derived(
    efforts.find((e) => e.effort === effort)?.effort ??
      selected?.defaultEffort ??
      efforts[0]?.effort ??
      '',
  );

  /** `ultracode` needs effort at xhigh or above — the CLI refuses it otherwise. */
  const ultracodeReady = $derived(currentEffort === 'xhigh' || currentEffort === 'max');

  function pickModel(event: Event) {
    const next = (event.currentTarget as HTMLSelectElement).value;
    const model = models.find((m) => m.id === next);
    // Snapped rather than carried over: the new model may not have the rung the old one was on.
    const keep = model?.efforts.some((e) => e.effort === currentEffort) === true;
    onchange({
      model: next,
      effort: keep
        ? currentEffort
        : (model?.defaultEffort ?? model?.efforts[0]?.effort ?? ''),
      flags,
    });
  }

  function pickEffort(event: Event) {
    const next = (event.currentTarget as HTMLSelectElement).value;
    onchange({
      model: selected?.id ?? '',
      effort: next,
      // A flag whose precondition the new effort breaks comes off, rather than being sent and
      // refused by the CLI with a message the user cannot act on.
      flags:
        next === 'xhigh' || next === 'max' ? flags : flags.filter((f) => f !== 'ultracode'),
    });
  }

  /**
   * A provider's flag key as something to put on screen.
   *
   * Capitalised and nothing more. The keys are provider-owned single words — `ultracode` is the only
   * one today — so a lookup table here would be this app inventing names for another program's
   * settings, and it would go stale silently the first time a CLI added one.
   */
  function label(name: string): string {
    return name.charAt(0).toUpperCase() + name.slice(1);
  }

  function toggleFlag(name: string, on: boolean) {
    onchange({
      model: selected?.id ?? '',
      effort: currentEffort,
      flags: on ? [...new Set([...flags, name])] : flags.filter((f) => f !== name),
    });
  }
</script>

<div class="c-model-picker">
  {#if capability === null}
    <p class="c-model-picker__note c-status--subtle">reading capabilities…</p>
  {:else if models.length === 0}
    <p class="c-model-picker__note c-status--warn">
      This agent reported no models. It may not be logged in.
    </p>
  {:else}
    <!--
      Two menu triggers in the `o-overlay-select` idiom, the fourth use after the title bar, the
      split button and the worktree bar's Links.

      A bare `<select>` sizes itself to its **widest option**, which is why this file used to need
      `max-width: 22ch` and an ellipsis: one long model id stretched the control past the pane. The
      object solves that at the source by drawing the label separately and stretching an invisible
      native select over it — so the trigger is the width of what is *selected*, the popup still
      renders outside the stacking context, and there is no third z-index.
    -->
    <span class="c-model-picker__trigger o-overlay-select" class:is-disabled={disabled}>
      <span class="c-model-picker__value" aria-hidden="true">
        {selected?.label ?? 'Model'}
        {#if !capability.modelsAreLive}
          <!-- Attached to the model, which is what it is about. Loose at the end of the row it
               landed next to the flag checkbox and read as that control's caption. -->
          <span
            class="c-model-picker__aside"
            title="wtm has no way to enumerate this agent's models">as of this build</span
          >
        {/if}
        <Icon name="chevron-down" size={11} />
      </span>
      <select
        class="o-overlay-select__native"
        aria-label="Model"
        value={selected?.id ?? ''}
        {disabled}
        onchange={pickModel}
        title={selected?.description ?? 'Model'}
      >
        {#each models as option (option.id)}
          <option value={option.id}>{option.label}</option>
        {/each}
      </select>
    </span>

    {#if efforts.length > 0}
      <span class="c-model-picker__trigger o-overlay-select" class:is-disabled={disabled}>
        <!-- The word is drawn, not hidden. Both labels used to be `u-visually-hidden`, so the row
             was two unnamed dropdowns — and it also makes the provider's raw `xhigh` and `max`
             legible in place without a display-name table for values the backend owns. -->
        <span class="c-model-picker__value" aria-hidden="true">
          <span class="c-model-picker__key">Effort</span>
          {currentEffort}
          <Icon name="chevron-down" size={11} />
        </span>
        <select
          class="o-overlay-select__native"
          aria-label="Effort"
          value={currentEffort}
          {disabled}
          onchange={pickEffort}
          title={efforts.find((e) => e.effort === currentEffort)?.description ?? 'Effort'}
        >
          {#each efforts as option (option.effort)}
            <option value={option.effort}>{option.effort}</option>
          {/each}
        </select>
      </span>
    {/if}

    {#each Object.entries(capability.flags) as [name, description] (name)}
      <Choice
        size="sm"
        checked={flags.includes(name)}
        disabled={disabled || (name === 'ultracode' && !ultracodeReady)}
        onchange={(on) => toggleFlag(name, on)}
      >
        <!-- The key, capitalised, with the sentence in the tooltip.
             `{name}` alone put the literal lowercase `ultracode` on screen, which was the
             complaint. Swapping in `description` overcorrected: it is a sentence, and at three
             lines it took over the whole toolbar and pushed the effort control onto its own row.
             A control needs a name; the explanation is what a tooltip is for. -->
        <span
          title={name === 'ultracode' && !ultracodeReady
            ? `${description} — needs effort at xhigh or max`
            : description}
        >
          {label(name)}
        </span>
      </Choice>
    {/each}
  {/if}
</div>

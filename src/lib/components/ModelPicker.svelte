<script lang="ts">
  /**
   * One control for a model and its effort, and the reason it is one control.
   *
   * **The effort ladder belongs to the model, not the provider.** Codex reports six efforts for
   * `gpt-5.6-sol` — including `ultra` — and four for `gpt-5.5`. A single effort list would therefore
   * offer rungs the selected model rejects, so changing the model changes the ladder, and picking a
   * model whose ladder lacks the current effort snaps to that model's own default.
   *
   * # Why native selects
   *
   * The same reason `OpenInButton` uses one: a native popup renders outside the stacking context, so
   * neither of these menus needs a `z-index` — and `settings/_config.scss` says the app has two
   * stacking levels and nothing else should add one. A hand-built listbox would have needed a third.
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
    <label class="c-model-picker__field">
      <span class="u-visually-hidden">Model</span>
      <select
        class="c-input c-input--sm"
        value={selected?.id ?? ''}
        {disabled}
        onchange={pickModel}
        title={selected?.description ?? 'Model'}
      >
        {#each models as option (option.id)}
          <option value={option.id}>{option.label}</option>
        {/each}
      </select>
    </label>

    <label class="c-model-picker__field">
      <span class="u-visually-hidden">Effort</span>
      <select
        class="c-input c-input--sm"
        value={currentEffort}
        {disabled}
        onchange={pickEffort}
        title={efforts.find((e) => e.effort === currentEffort)?.description ?? 'Effort'}
      >
        {#each efforts as option (option.effort)}
          <option value={option.effort}>{option.effort}</option>
        {/each}
      </select>
    </label>

    {#each Object.entries(capability.flags) as [name, description] (name)}
      <Choice
        size="sm"
        checked={flags.includes(name)}
        disabled={disabled || (name === 'ultracode' && !ultracodeReady)}
        onchange={(on) => toggleFlag(name, on)}
      >
        <span
          title={name === 'ultracode' && !ultracodeReady
            ? 'Needs xhigh or max'
            : description}
        >
          {name}
        </span>
      </Choice>
    {/each}

    {#if !capability.modelsAreLive}
      <!-- Said rather than implied. A stale list being this build's fault and being the CLI's are
           different problems, and only one of them is fixable by the user. -->
      <span
        class="c-model-picker__note c-status--subtle"
        title="wtm has no way to enumerate this agent's models"
      >
        as of this build
      </span>
    {/if}
  {/if}
</div>

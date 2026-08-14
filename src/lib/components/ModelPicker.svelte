<script lang="ts">
  /**
   * One control for a model and its effort, and the reason it is one control.
   *
   * # The agent is part of the model menu, not a separate control
   *
   * Because "which agent" and "which model" are the same question asked at two grains, and treating
   * them as two controls made the second one unreachable: this component used to know only the
   * running provider's list, so the Split button produced another pane of the same agent and the only
   * routes to the other one were the empty-state launcher and the Plans hand-off. Grouping both
   * catalogues under `<optgroup>`s costs no new control, no new stacking level, and no new vocabulary
   * — a model id already implied its provider, and now the menu says so.
   *
   * The cost is that picking across providers cannot apply to a running session, because **the
   * provider is the process**. So it is marked "on restart", exactly as effort is, and `restart`
   * adopts it. A pane nobody has asked anything yet skips the marker and just becomes the other
   * agent; see `Sessions.configure`.
   *
   * **The effort ladder belongs to the model, not the provider.** Codex reports six efforts for
   * `gpt-5.6-sol` — including `ultra` — and four for `gpt-5.5`. A single effort list would therefore
   * offer rungs the selected model rejects, so changing the model changes the ladder, and picking a
   * model whose ladder lacks the current effort snaps to that model's own default.
   *
   * # Why native selects, worn under a drawn label
   *
   * Native, for the same reason `OpenInButton` is: the popup renders outside the stacking context, so
   * neither menu needs a `z-index` — and `settings/_config.scss` enumerates every stacking level the
   * app has, on the rule that one chosen locally is one nobody can reason about. A hand-built listbox
   * would have needed one of its own, and now a two-level one would.
   *
   * Drawn label, because a `<select>` is as wide as its **widest option** rather than its selected
   * one. As plain form controls these two were sized by the longest model id in the list and had to
   * be capped and ellipsized to fit a narrow pane — the cap then truncating the *selected* model,
   * which is the one thing the control exists to show. `o-overlay-select` is the app's existing
   * answer; `_input.scss` draws the line these two were on the wrong side of.
   *
   * # The mode pill is the one control here that changes colour with its value
   *
   * Everything else in this row is deliberately uniform — value-first, quiet until pointed at — and
   * breaking that for one control needs a reason. This is it: the permission mode is the most
   * consequential setting in the app, it persists silently across every turn, and it is the only
   * one whose wrong value can cost you something you cannot undo. A pane left in
   * `bypassPermissions` overnight and a pane in `Manual` must not look the same.
   *
   * The colour is reinforcement and never the signal — `settings/_semantic.scss` forbids that, and
   * this is the case the rule most obviously exists for. The **label changes too**, the risk tier
   * comes from the backend rather than a substring test here, and the tier itself is provider
   * independent: Codex's `full-access` and Claude's `bypassPermissions` are both `unsandboxed`
   * without this file knowing what either word means.
   *
   * # There are no flag checkboxes any more
   *
   * There was one, `ultracode`, and it never did anything — no `flags` field existed on the request
   * that reaches the CLI, so the value died here. It is now the top rung of the effort ladder,
   * which is where the CLI's own `/effort` menu puts it, and `claude.rs` translates it into the
   * settings key that actually turns it on. Codex's `ultra` is a different thing with a similar
   * name — a real rung on some of its models, and it arrives in that provider's own effort list.
   */
  import type { Capability } from '../ipc/types';
  import Icon from './ui/Icon.svelte';

  /** One provider's entry in the model menu. */
  export interface ModelGroup {
    id: string;
    label: string;
    capability: Capability | null;
  }

  const {
    providers,
    provider,
    pendingProvider,
    model,
    effort,
    mode,
    effortPending = false,
    disabled = false,
    onchange,
  }: {
    /** Every agent this repository and machine can start, in catalogue order. */
    providers: ModelGroup[];
    /** The provider the running session **is**. */
    provider: string;
    /** A provider chosen here that the session is not. Null when the two agree. */
    pendingProvider: string | null;
    model: string | null;
    effort: string | null;
    /** The permission mode, in the provider's spelling. Null before the session reports one. */
    mode: string | null;
    /** Effort has been changed and the running session is not using it yet. */
    effortPending?: boolean;
    disabled?: boolean;
    onchange: (next: {
      provider: string;
      model: string;
      effort: string;
      mode: string | null;
    }) => void;
  } = $props();

  /** The provider whose models, ladder and labels this control is showing. */
  const chosen = $derived(pendingProvider ?? provider);
  const chosenGroup = $derived(providers.find((p) => p.id === chosen) ?? null);
  const capability = $derived(chosenGroup?.capability ?? null);

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

  /**
   * The modes of the provider that is **running**, not of the one the menu is pointed at.
   *
   * The one place in this component that deliberately ignores `chosen`, and the reason is the
   * paragraph above about why this pill is the only one that changes colour. The mode is the setting
   * applied *live* and the only one whose wrong value can cost something irreversible, so the pill
   * has to describe the process that exists. Following a pending provider would offer Codex's
   * `full-access` to a Claude session and paint the `unsandboxed` tint on a mode nothing is in —
   * which is exactly the "colour as the signal" failure `_semantic.scss` forbids, arrived at from
   * the other direction.
   */
  const runningCapability = $derived(
    providers.find((p) => p.id === provider)?.capability ?? null,
  );
  const modes = $derived(runningCapability?.modes ?? []);
  /**
   * The mode on screen.
   *
   * Falls through to the capability's default and then to nothing — not to the first entry, unlike
   * the model above. Claude marks no default deliberately, because wtm passes no
   * `--permission-mode` and lets `~/.claude/settings.json` decide; picking `modes[0]` here would
   * put a confident "Manual" on the pill during the second before `session_ready` says otherwise,
   * and that second is exactly when someone might glance at it.
   */
  const currentMode = $derived(
    modes.find((m) => m.id === mode) ?? modes.find((m) => m.isDefault) ?? null,
  );

  /**
   * Split an option value back into the provider it came from and the model itself.
   *
   * The value is `provider:model` because a model id alone does not say which list it was in — and
   * with two providers in one menu, that is now ambiguous rather than merely redundant. `indexOf`
   * rather than `split`, so a model id containing a colon survives being selected.
   */
  function parseOption(value: string): { provider: string; model: string } {
    const at = value.indexOf(':');
    if (at < 0) return { provider: chosen, model: value };
    return { provider: value.slice(0, at), model: value.slice(at + 1) };
  }

  function pickModel(event: Event) {
    const next = parseOption((event.currentTarget as HTMLSelectElement).value);
    const group = providers.find((p) => p.id === next.provider);
    const model = group?.capability?.models.find((m) => m.id === next.model);
    // Snapped rather than carried over: the new model may not have the rung the old one was on. Now
    // doubly so — across providers the ladders are different lengths, not just different defaults.
    const keep = model?.efforts.some((e) => e.effort === currentEffort) === true;
    onchange({
      provider: next.provider,
      model: next.model,
      effort: keep
        ? currentEffort
        : (model?.defaultEffort ?? model?.efforts[0]?.effort ?? ''),
      // Untouched. `restart` is what drops a mode that does not cross, because it is the thing that
      // knows the swap actually happened — cancelling it here would also cancel it for a swap the
      // user then retracts. The one exception — a model that implies a mode, like `opusplan` —
      // lives in `sessions.configure()`, which sees every route to a model change, not just this one.
      mode,
    });
  }

  function pickEffort(event: Event) {
    onchange({
      provider: chosen,
      model: selected?.id ?? '',
      effort: (event.currentTarget as HTMLSelectElement).value,
      mode,
    });
  }

  function pickMode(event: Event) {
    // `chosen`, so changing the mode of a running session does not silently retract a pending swap.
    onchange({
      provider: chosen,
      model: selected?.id ?? '',
      effort: currentEffort,
      mode: (event.currentTarget as HTMLSelectElement).value,
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
    <span
      class="c-model-picker__trigger o-overlay-select"
      class:is-disabled={disabled}
      class:is-pending={pendingProvider !== null}
    >
      <span class="c-model-picker__value" aria-hidden="true">
        {selected?.label ?? 'Model'}
        {#if pendingProvider !== null}
          <!-- The same two words the effort pill uses, because it is the same fact and the same
               mechanism: the provider is the process, so a running session cannot be told about it.
               Never both asides at once — "as of this build" belongs to the *selected* group, and a
               pending swap says the more urgent thing about the same control. -->
          <span class="c-model-picker__aside">on restart</span>
        {:else if !capability.modelsAreLive}
          <!-- Attached to the model, which is what it is about. Loose at the end of the row it
               landed next to the flag checkbox and read as that control's caption. -->
          <span
            class="c-model-picker__aside"
            title="wtm has no way to enumerate this agent's models">as of this build</span
          >
        {/if}
        <Icon name="chevron-down" size={11} />
      </span>
      <!--
        Every agent's models in one menu, grouped.

        Splitting the chat used to mean splitting into the *same* agent, because this control only
        ever knew the running provider's list — so the only routes to a second model were the empty
        state and the Plans hand-off. One menu is the honest shape: the question "what runs this
        turn" has always had two halves, and presenting them as one control is what makes the second
        half discoverable.

        `<optgroup>` needs no `z-index`, for exactly the reason the header gives for these being
        native selects at all: the popup renders outside the stacking context. A hand-built two-level
        menu would have needed the third stacking level `settings/_config.scss` forbids.
      -->
      <select
        class="o-overlay-select__native"
        aria-label="Model"
        value={selected ? `${chosen}:${selected.id}` : ''}
        {disabled}
        onchange={pickModel}
        title={pendingProvider !== null
          ? `Restart to start a ${chosenGroup?.label ?? pendingProvider} session on ${selected?.label ?? 'this model'}`
          : (selected?.description ?? 'Model')}
      >
        {#each providers as group (group.id)}
          {#if group.capability !== null && group.capability.models.length > 0}
            <!-- The provenance caveat rides on the group label rather than the trigger, because one
                 menu now mixes a list Codex reported with a table compiled into this build, and
                 "as of this build" said loosely would claim it of both. -->
            <optgroup
              label={group.capability.modelsAreLive
                ? group.label
                : `${group.label} — as of this build`}
            >
              {#each group.capability.models as option (option.id)}
                <option value="{group.id}:{option.id}">{option.label}</option>
              {/each}
            </optgroup>
          {/if}
        {/each}
      </select>
    </span>

    {#if efforts.length > 0}
      <span
        class="c-model-picker__trigger o-overlay-select"
        class:is-disabled={disabled}
        class:is-pending={effortPending}
      >
        <!-- The word is drawn, not hidden. Both labels used to be `u-visually-hidden`, so the row
             was two unnamed dropdowns — and it also makes the provider's raw `xhigh` and `max`
             legible in place without a display-name table for values the backend owns. -->
        <span class="c-model-picker__value" aria-hidden="true">
          <span class="c-model-picker__key">Effort</span>
          {currentEffort}
          {#if effortPending}
            <!-- A word, and *only* a word: the `is-pending` hook is deliberately unstyled, and
                 `_model-picker.scss` keeps the reason a decoration was tried and removed. One of two
                 settings a running session cannot be told about — `--effort` is argv, read once,
                 with no control request for it. Saying so is the honest alternative to restarting
                 behind the user's back or greying the control out entirely. The model pill says the
                 same two words for the other one. -->
            <span class="c-model-picker__aside">on restart</span>
          {/if}
          <Icon name="chevron-down" size={11} />
        </span>
        <select
          class="o-overlay-select__native"
          aria-label="Effort"
          value={currentEffort}
          {disabled}
          onchange={pickEffort}
          title={effortPending
            ? 'Restart the session to apply this effort'
            : (efforts.find((e) => e.effort === currentEffort)?.description ?? 'Effort')}
        >
          {#each efforts as option (option.effort)}
            <option value={option.effort}>{option.effort}</option>
          {/each}
        </select>
      </span>
    {/if}

    {#if modes.length > 0}
      <!-- The risk tier rides on the wrapper, so the pill's own fill and text colour change with
           it. `is-` states are always chained per the CSS rules, and the three are mutually
           exclusive by construction — `risk` is an enum, not a set of booleans. -->
      <span
        class="c-model-picker__trigger o-overlay-select"
        class:is-disabled={disabled}
        class:is-elevated={currentMode?.risk === 'elevated'}
        class:is-unsandboxed={currentMode?.risk === 'unsandboxed'}
      >
        <span class="c-model-picker__value" aria-hidden="true">
          <!-- No standing "Mode" word beside it, unlike Effort. A mode's label is a phrase that
               already says what it is — "Accept edits", "Bypass permissions" — where `xhigh` on its
               own is a value in search of a noun. The em dash is the placeholder for the second
               before a session reports which mode it resolved to. -->
          {currentMode?.label ?? '—'}
          <Icon name="chevron-down" size={11} />
        </span>
        <select
          class="o-overlay-select__native"
          aria-label="Permission mode"
          value={currentMode?.id ?? ''}
          {disabled}
          onchange={pickMode}
          title={currentMode?.description ?? 'Permission mode'}
        >
          {#if currentMode === null}
            <!-- A sentinel, because a `<select>` with no matching value shows its first option and
                 would claim the session is in a mode nobody chose. Never selectable back. -->
            <option value="" disabled>—</option>
          {/if}
          {#each modes as option (option.id)}
            <option value={option.id}>{option.label}</option>
          {/each}
        </select>
      </span>
    {/if}
  {/if}
</div>

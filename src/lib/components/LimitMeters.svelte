<script lang="ts">
  /**
   * One provider account's usage limits: a row and a bar per window, then when it was read.
   *
   * Shared by the usage dialog and a pane's context card, so the two never word the same figure two
   * ways. Draws only what it is given: asking the provider is the caller's decision, because the
   * two places ask at different moments (see `usage.svelte.ts`).
   */
  import type { AccountUsage } from '../ipc/types';
  import {
    elapsed,
    limitLabel,
    noFiguresText,
    resetText,
    toneFor,
    updatedText,
  } from '../state/usage.svelte';
  import Meter from './ui/Meter.svelte';

  const {
    provider,
    label,
    account,
    asking = false,
    now,
  }: {
    provider: string;
    /** The provider as the user knows it, such as "Claude". */
    label: string;
    account: AccountUsage | undefined;
    asking?: boolean;
    /**
     * The moment to judge resets against, taken by the caller when it opened.
     *
     * A prop rather than read here, because nothing redraws this on a clock: a caller that took it
     * once and passes it down keeps every row agreeing about what "now" was.
     */
    now: number;
  } = $props();

  const windows = $derived(account?.limits.windows ?? []);
  /** When the figures are from, and Codex's unused free resets, joined into one quiet line. */
  const footnote = $derived.by(() => {
    const parts: string[] = [];
    if (account?.reportedAt != null) parts.push(updatedText(account.reportedAt, now));
    const credits = account?.limits.resetCredits ?? 0;
    if (credits > 0)
      parts.push(`${credits} free ${credits === 1 ? 'reset' : 'resets'} available`);
    return parts.join(' · ');
  });
</script>

{#if windows.length > 0}
  <ul class="o-plain-list c-usage__windows">
    {#each windows as window (`${window.minutes}:${window.scope}`)}
      {@const over = elapsed(window, now)}
      <li class="c-usage__window">
        <span class="c-usage__name">{limitLabel(window)}</span>
        <span class="c-usage__reset">{resetText(window, now)}</span>
        <!-- An elapsed window shows no figure: after a reset the true one is lower by an amount
             nobody has reported yet, and the old one would be a confident wrong number. -->
        <span class="c-usage__figure"
          >{over ? '—' : `${Math.round(window.usedPercent)}%`}</span
        >
        <div class="c-usage__bar">
          <Meter
            percent={over ? null : window.usedPercent}
            tone={over ? 'accent' : toneFor(window.usedPercent)}
            label="{limitLabel(window)} used"
          />
        </div>
      </li>
    {/each}
  </ul>
{:else if asking}
  <p class="c-usage__note">Asking {label}…</p>
{:else if !account?.error}
  <p class="c-usage__note">{noFiguresText(provider, label)}</p>
{/if}

{#if account?.error}
  <p class="c-usage__error">{label} could not be asked: {account.error}</p>
{/if}

{#if footnote}
  <p class="c-usage__note">{footnote}</p>
{/if}

<script lang="ts">
  /**
   * A role waiting at its provider's usage limit, and the three things you can do about it: keep
   * waiting, run it once on another provider, or finish the review without it. The same card in
   * the Reviews tab and in the inbox, so choosing in either place chooses.
   *
   * # The usage line is the provider's, or unknown
   *
   * The figure beside the reset is whatever the provider last reported to wtm — a session's own
   * report, or the usage view's ask — and says when. With nothing reported it says "unknown",
   * never an estimate: the plan's rule for usage is real data or nothing.
   *
   * # Run once is a choice of provider and model
   *
   * The picker offers the other provider by default, since the one that ran out would meet the same
   * limit, and its models as that provider's CLI lists them. A model that bills usage credits is
   * marked, and Rust refuses it for a role that has not allowed it (`allow_credit_billing`); the
   * card says why before you ask, and shows Rust's refusal if you ask anyway.
   */
  import type { CapacityChoice, CapacityView, RoleProvider } from '../ipc/types';
  import { providerName, resetTime } from '../review-run';
  import { sessions } from '../state/sessions.svelte';
  import { summaryText, tightest, updatedText, usage } from '../state/usage.svelte';
  import CodeText from './CodeText.svelte';
  import Button from './ui/Button.svelte';

  const {
    capacity,
    busy,
    error,
    onchoose,
    ontry,
  }: {
    capacity: CapacityView;
    busy: boolean;
    error: string | null;
    onchoose: (choice: CapacityChoice) => void;
    /** Run the role now, before the reset, which may meet the same limit. */
    ontry: () => void;
  } = $props();

  const PROVIDERS: RoleProvider[] = ['claude', 'codex'];
  const other = $derived<RoleProvider>(capacity.provider === 'claude' ? 'codex' : 'claude');
  const limited = $derived(providerName(capacity.provider));

  let picking = $state(false);
  let provider = $state<RoleProvider | null>(null);
  let model = $state('');
  const chosen = $derived<RoleProvider>(provider ?? other);

  $effect(() => {
    if (picking) void sessions.loadCapability(chosen);
  });

  const models = $derived(sessions.capabilities[chosen]?.models ?? []);
  /** The docs' rule, as `bills_credits` holds it in Rust: Fable, and the alias that resolves to it. */
  const billsCredits = (provider: RoleProvider, id: string) =>
    provider === 'claude' &&
    (id.toLowerCase() === 'best' || id.toLowerCase().includes('fable'));
  const refusedForCredits = $derived(
    billsCredits(chosen, model) && !capacity.allowCreditBilling,
  );
  const sameAsLimited = $derived(
    chosen === capacity.provider && (model || null) === (capacity.model ?? null),
  );

  const now = Date.now();
  const account = $derived(usage.accounts[capacity.provider]);
  const worst = $derived(tightest(account, now));
  const usageLine = $derived(
    worst && account?.reportedAt
      ? `${summaryText(worst)} · ${updatedText(account.reportedAt, now)}`
      : `unknown: ${limited} has not reported figures wtm can show`,
  );
  const resets = $derived(resetTime(capacity.resetsAt));

  function runOnce(): void {
    onchoose({ kind: 'run_once', provider: chosen, model: model || null });
  }
</script>

<section
  class="c-work-state c-work-state--warn c-capacity"
  aria-labelledby="work-capacity-{capacity.step}"
>
  <span class="c-work-state__eyebrow">Usage limit · {limited}</span>
  <h3 class="c-work-state__title" id="work-capacity-{capacity.step}">
    <CodeText
      text={`Wait for the reset, run \`${capacity.role}\` once elsewhere, or finish without it?`}
    />
  </h3>
  <dl class="o-facts c-capacity__facts">
    <dt>Resets</dt>
    <dd>
      {resets
        ? `${resets}, then it runs again by itself while wtm is open and the Mac is awake`
        : `${limited} did not say, so wtm waits for you rather than guess`}
    </dd>
    <dt>{limited} usage</dt>
    <dd>{usageLine}</dd>
    <dt>Fallbacks</dt>
    <dd>
      {#if capacity.fallbacks.length === 0}
        None configured for this role, so nothing switches by itself.
      {:else}
        <ul class="o-plain-list c-capacity__chain">
          {#each capacity.fallbacks as fallback (fallback.role)}
            <li>
              <code>{fallback.role}</code> on {providerName(
                fallback.provider,
              )}{fallback.model ? ` ${fallback.model}` : ''}{fallback.passedOver
                ? ` · passed over: ${fallback.passedOver}`
                : ''}
            </li>
          {/each}
        </ul>
      {/if}
    </dd>
    {#if capacity.recheck}
      <dt>Last check</dt>
      <dd>{capacity.recheck}</dd>
    {/if}
  </dl>
  <p class="c-work-state__note">
    Nothing switches provider by itself. After a sleep or a relaunch, wtm asks {limited} again
    before running it. wtm never uses an API key.
  </p>

  {#if picking}
    <form
      class="c-capacity__once"
      onsubmit={(event) => {
        event.preventDefault();
        runOnce();
      }}
    >
      <label class="c-work-state__field">
        Provider
        <select
          class="c-select"
          value={chosen}
          onchange={(event) => {
            provider = event.currentTarget.value as RoleProvider;
            model = '';
          }}
        >
          {#each PROVIDERS as option (option)}
            <option value={option}
              >{providerName(option)}{option === capacity.provider
                ? ' (the one that ran out)'
                : ''}</option
            >
          {/each}
        </select>
      </label>
      <label class="c-work-state__field">
        Model
        <select
          class="c-select"
          value={model}
          onchange={(event) => (model = event.currentTarget.value)}
        >
          <option value="">{providerName(chosen)}'s default</option>
          {#each models as option (option.id)}
            <option value={option.id}
              >{option.label}{billsCredits(chosen, option.id)
                ? ' · bills usage credits'
                : ''}</option
            >
          {/each}
        </select>
      </label>
      <p class="c-work-state__note">
        It runs from a written handover — the step, the snapshot, the diff's shape, the
        findings so far and your answers. No conversation carries over. A rerun goes back to {limited}.
      </p>
      {#if refusedForCredits}
        <p class="c-capacity__warning">
          <CodeText
            text={`This model bills usage credits in a headless run, and \`${capacity.role}\` has not allowed that. Add \`allow_credit_billing = true\` to its role in your config, or pick another model.`}
          />
        </p>
      {/if}
      <div class="c-work-state__actions">
        <Button variant="accent" size="sm" type="submit" disabled={busy || sameAsLimited}
          >Run once on {providerName(chosen)}{model ? ` ${model}` : ''}</Button
        >
        <Button variant="quiet" size="sm" onclick={() => (picking = false)}>Cancel</Button>
      </div>
    </form>
  {:else}
    <div class="c-work-state__actions">
      <Button
        variant="accent"
        size="sm"
        disabled={busy}
        onclick={() => onchoose({ kind: 'wait' })}>Wait and resume</Button
      >
      <Button size="sm" disabled={busy} onclick={() => (picking = true)}
        >Run this role on {providerName(other)} once…</Button
      >
      <Button
        variant="quiet"
        size="sm"
        disabled={busy}
        onclick={() => onchoose({ kind: 'skip' })}>Finish without it</Button
      >
      <Button variant="quiet" size="sm" disabled={busy} onclick={ontry}>Try now</Button>
    </div>
  {/if}
  {#if error}
    <p class="c-work-state__error" role="alert"><CodeText text={error} /></p>
  {/if}
</section>

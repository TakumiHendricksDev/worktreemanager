<script lang="ts">
  /**
   * Offering an update, and saying plainly what installing it will do to what is on screen.
   *
   * # Why the warning names a number
   *
   * "Running sessions will be stopped" is the sentence people click through. "2 agents are
   * mid-turn" is one they read, because it is about something they can see. So the count is live
   * — a turn that finishes while the dialog is open drops out of it — and the sentence is absent
   * when it would say zero.
   *
   * # Two kinds of install, two dialogs' worth of advice
   *
   * Homebrew can upgrade a copy it installed, so that copy gets a button. Any other copy gets
   * instructions. They used to be mostly about browsers, because a browser quarantines what it
   * downloads and macOS called an unnotarized wtm "damaged". Releases are notarized now, so the
   * release page is safe to send someone to as it is.
   */
  import { commands } from '../ipc/commands';
  import { sessions } from '../state/sessions.svelte';
  import { updates } from '../state/update.svelte';
  import Button from './ui/Button.svelte';
  import Dialog from './ui/Dialog.svelte';

  /** For a copy that did not come from the tap. `--force` because the app is already there. */
  const SWITCH_TO_BREW = 'brew install --cask --force takumihendricksdev/tap/wtm';
  /** What the dialog would have done, for running by hand after it failed. */
  const UPGRADE_BY_HAND = 'brew update && brew upgrade --cask takumihendricksdev/tap/wtm';

  const status = $derived(updates.status);
  const busy = $derived(updates.phase !== 'idle');
  const working = $derived(sessions.panes.filter((p) => p.working).length);
  const failure = $derived(updates.outcome?.kind === 'failed' ? updates.outcome : null);

  function openPage(): void {
    if (status) void commands.openUrl(status.url).catch(() => {});
  }
</script>

{#if updates.dialog === 'failure' && failure}
  <Dialog title="The update didn’t finish" onclose={() => updates.clearOutcome()}>
    {#snippet body()}
      <p>
        wtm quit to update to {failure.version}, but came back without it. The end of what
        Homebrew said:
      </p>
      <pre class="c-update__log">{failure.log || '(Homebrew wrote nothing.)'}</pre>
      <p>You can run the same upgrade in Terminal:</p>
      <div class="c-update__command">{UPGRADE_BY_HAND}</div>
      <p class="c-note">
        If the log says “Operation not permitted”, macOS stopped wtm replacing an app. Allow
        Worktree Manager under System Settings → Privacy &amp; Security → App Management, or
        run the command above.
      </p>
    {/snippet}
    {#snippet footer()}
      <Button variant="neutral" onclick={() => updates.clearOutcome()}>Close</Button>
    {/snippet}
  </Dialog>
{:else if updates.dialog === 'update'}
  <Dialog
    title={status?.available ? 'Update available' : 'Check for updates'}
    onclose={() => updates.close()}
    closeDisabled={busy}
  >
    {#snippet body()}
      {#if updates.checking}
        <p>Checking for updates…</p>
      {:else if !status?.available}
        {#if updates.error}
          <p class="c-status--danger">{updates.error}</p>
        {:else if status}
          <p>You’re up to date. {status.current} is the latest release.</p>
        {/if}
        {#if !updates.enabled}
          <p class="c-note">
            Automatic checks are off. Turn them on under Settings → General.
          </p>
        {/if}
      {:else}
        <p>
          Worktree Manager {status.latest} is out. You have {status.current}.
          <Button variant="link" onclick={openPage}>Release notes</Button>
        </p>

        {#if status.via === 'homebrew'}
          <p>
            Updating quits wtm, lets Homebrew install the new version, and opens it again.
            Your panes come back where they were, and agent conversations can be picked up
            where you left off.
          </p>
          {#if working > 0}
            <p class="c-status--warn">
              {working === 1 ? 'One agent is' : `${working} agents are`} in the middle of a turn,
              which will be stopped.
            </p>
          {/if}
          <p class="c-note">Terminals close too, along with anything running in them.</p>
          {#if updates.phase === 'preparing'}
            <p class="c-note">
              Downloading with Homebrew. wtm stays open until that is done, which can take a
              minute.
            </p>
          {/if}
        {:else}
          <p>
            This copy wasn’t installed by Homebrew, so wtm can’t update it for you. Download
            the new version from the release page and move it to Applications in place of
            this one.
          </p>
          <p>Or switch to Homebrew, and wtm can update itself from then on:</p>
          <div class="c-update__command">{SWITCH_TO_BREW}</div>
        {/if}

        {#if updates.error}<p class="c-status--danger">{updates.error}</p>{/if}
      {/if}
    {/snippet}

    {#snippet footer()}
      {#if status?.available && !updates.checking}
        <Button variant="quiet" onclick={() => void updates.skip()} disabled={busy}
          >Skip this version</Button
        >
        <Button variant="neutral" onclick={() => updates.close()} disabled={busy}
          >Later</Button
        >
        {#if status.via === 'homebrew'}
          <Button variant="accent" onclick={() => void updates.install()} disabled={busy}>
            {updates.phase === 'preparing'
              ? 'Downloading…'
              : updates.phase === 'installing'
                ? 'Restarting…'
                : 'Update and restart'}
          </Button>
        {:else}
          <Button variant="accent" onclick={openPage}>Open release page</Button>
        {/if}
      {:else}
        <Button variant="neutral" onclick={() => updates.close()}>Done</Button>
      {/if}
    {/snippet}
  </Dialog>
{/if}

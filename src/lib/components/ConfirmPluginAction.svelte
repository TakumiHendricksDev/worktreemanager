<script lang="ts">
  /**
   * The question before a plugin change that is hard to take back: uninstall, roll back, plugin
   * recovery, hiding a region the window cannot do without.
   *
   * One component so each of them gets the same three things, which the single shared dialog
   * gave none of: the app's danger button for a destructive answer (`CloseSessionDialog`,
   * `AgentsDialog`), a busy label that says what is happening, and its failure shown in the
   * dialog that caused it rather than on the page behind it as well.
   */
  import type { Snippet } from 'svelte';
  import { PluginTask } from '../state/plugins.svelte';
  import Banner from './ui/Banner.svelte';
  import Button from './ui/Button.svelte';
  import Dialog from './ui/Dialog.svelte';

  const {
    title,
    confirm,
    working,
    tone = 'danger',
    action,
    ondone,
    onclose,
    children,
  }: {
    title: string;
    /** The confirming button's label. */
    confirm: string;
    /** Its label while `action` runs: "Uninstalling…". */
    working: string;
    /** `danger` for a change that removes or turns off; `accent` for one that only rearranges. */
    tone?: 'danger' | 'accent';
    action: () => Promise<unknown>;
    /** Called once `action` succeeds; the caller closes the dialog and reports the outcome. */
    ondone: () => void;
    onclose: () => void;
    /** What the change does, in full. */
    children: Snippet;
  } = $props();

  const task = new PluginTask();

  async function confirmed() {
    if (await task.run(action)) ondone();
  }
</script>

<Dialog {title} onclose={() => !task.busy && onclose()} closeDisabled={task.busy}>
  {#snippet body()}
    <div class="o-stack">
      {@render children()}
      {#if task.error}<Banner>{task.error}</Banner>{/if}
    </div>
  {/snippet}
  {#snippet footer()}
    <Button disabled={task.busy} onclick={onclose}>Cancel</Button>
    <Button
      variant={tone === 'danger' ? 'danger-solid' : 'accent'}
      disabled={task.busy}
      onclick={() => void confirmed()}>{task.busy ? working : confirm}</Button
    >
  {/snippet}
</Dialog>

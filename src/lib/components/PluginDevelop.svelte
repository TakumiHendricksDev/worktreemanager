<script lang="ts">
  /**
   * Settings → Plugins → Develop a plugin: start one with an agent, or link a folder you write
   * yourself, and look up the public contract while you do.
   *
   * The contract inventory is one list, folded away until asked for. It was rendered twice, open,
   * about six hundred rows each with a search box of its own, so the page's own instructions sat
   * between two copies of a reference most visits do not need.
   */
  import { onMount } from 'svelte';
  import { commands } from '../ipc/commands';
  import { plugins } from '../state/plugins.svelte';
  import Banner from './ui/Banner.svelte';
  import Button from './ui/Button.svelte';
  import PluginContracts from './PluginContracts.svelte';

  const { oncreate, onlink }: { oncreate: () => void; onlink: () => void } = $props();

  const starter = `schema = 1\napi = 1\nid = "example.project-tools"\nname = "Project Tools"\nversion = "0.1.0"\n\n[[commands]]\nid = "summary"\ntitle = "Show project summary"\ncommand = ["git", "status", "--short"]`;
  let contracts = $state(false);
  let notice = $state<string | null>(null);

  onMount(() => {
    let gone = false;
    // A notice is only ever about a damaged drafts file that was set aside, so failing to ask is
    // not worth a banner of its own.
    void commands
      .pluginDraftsNotice()
      .then((text) => {
        if (!gone) notice = text;
      })
      .catch(() => {});
    return () => {
      gone = true;
    };
  });
</script>

<div class="c-plugins__content o-stack o-stack--loose">
  {#if notice && !plugins.draftsNoticeDismissed}
    <Banner variant="warn"
      >{notice}{#snippet action()}<Button
          size="sm"
          variant="quiet"
          onclick={() => (plugins.draftsNoticeDismissed = true)}>Dismiss</Button
        >{/snippet}</Banner
    >
  {/if}
  <h3 class="c-pane-title">Build a plugin with an agent</h3>
  <p class="u-muted">
    Describe a change and keep refining it in its own Claude or Codex conversation.
  </p>
  <div><Button variant="accent" onclick={oncreate}>Create with an agent…</Button></div>
  <div class="c-plugins__section o-stack">
    <h3 class="c-pane-title">A folder is a plugin</h3>
    <p class="u-muted">
      Put a wtm-plugin.toml in a folder. It can add styles, actions, settings, content and
      behaviour inside wtm's own screens, or declare commands, panels, replacement views and
      session backends. The public contract below lists what you can build on.
    </p>
    <pre class="c-plugins__code">{starter}</pre>
    <p class="c-note">
      Start from the example plugins in examples/plugins and the guide in docs/plugins.md in
      the wtm source. Session backends speak versioned JSON-RPC over stdin and stdout; yours
      can use SSH to reach a session host that stays up.
    </p>
    <div><Button variant="accent" onclick={onlink}>Link development folder…</Button></div>
    <p class="c-note">
      Validate, review what it can do, turn it on, then reload and review after each edit.
      wtm runs the snapshot you reviewed, so editing a linked folder never changes trusted
      code by itself.
    </p>
  </div>
  <div class="c-plugins__section o-stack">
    <h4 class="c-section-heading">Public contract</h4>
    <div>
      <Button
        ariaExpanded={contracts}
        ariaControls="plugin-contracts"
        onclick={() => (contracts = !contracts)}
        >{contracts ? 'Hide the public contract' : 'Show the public contract'}</Button
      >
    </div>
    {#if contracts}<PluginContracts id="plugin-contracts" />{/if}
  </div>
</div>

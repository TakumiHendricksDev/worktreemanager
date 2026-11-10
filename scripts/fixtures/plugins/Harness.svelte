<script lang="ts">
  /**
   * The plugins fixture: Settings → Plugins, and a chat page that renders real contributions.
   *
   * The chat page mounts through `PluginSlot`, as `App` mounts the whole workbench, because the
   * slot once marked everything under it protected and no contribution rendered in the real window
   * while every component mounted directly here passed.
   */
  import { paneContext, provideExtensionContext } from '/src/lib/state/extensions.svelte';
  import PluginSettings from '/src/lib/components/PluginSettings.svelte';
  import PluginSlot from '/src/lib/components/PluginSlot.svelte';
  import AgentTranscript from '/src/lib/components/AgentTranscript.svelte';
  import ChatEditDialog from '/src/lib/components/ChatEditDialog.svelte';
  import ExtensionPoint from '/src/lib/components/ExtensionPoint.svelte';
  import Button from '/src/lib/components/ui/Button.svelte';

  provideExtensionContext(() =>
    paneContext({
      id: 'qa-pane',
      projectId: 'qa-project',
      worktreeId: '/tmp/qa-workspace',
      session: 'qa-session',
    }),
  );
  let page = $state('plugins');
  const events = [
    { kind: 'history_boundary', complete: true },
    { kind: 'user_echo', text: 'Use links in this reply.' },
    {
      kind: 'message_identity',
      message: 'm1',
      turn: 't1',
      text: 'Use links in this reply.',
      attachments: false,
    },
    { kind: 'message', text: 'Read the [project guide](https://example.com/guide).' },
  ];
</script>

<nav>
  <Button onclick={() => (page = 'plugins')}>Plugins</Button><Button
    onclick={() => (page = 'chat')}>Chat fixture</Button
  >
</nav>
<p>Isolated UI verification · example transcript and mocked host responses</p>
{#if page === 'plugins'}<PluginSettings />{:else}
  <section aria-label="Through a plugin slot">
    <PluginSlot region="workbench">
      <ExtensionPoint target="workspace.toolbar" position="after" />
      <AgentTranscript events={events as any} />
      <ExtensionPoint target="settings.appearance" position="after" />
    </PluginSlot>
  </section>
{/if}
<ChatEditDialog />

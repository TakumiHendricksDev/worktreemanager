<script lang="ts">
  import { setContext } from 'svelte';
  import { EXTENSION_CONTEXT } from '/src/lib/state/extensions.svelte';
  import PluginSettings from '/src/lib/components/PluginSettings.svelte';
  import AgentTranscript from '/src/lib/components/AgentTranscript.svelte';
  import ChatEditDialog from '/src/lib/components/ChatEditDialog.svelte';
  import ExtensionPoint from '/src/lib/components/ExtensionPoint.svelte';
  import Button from '/src/lib/components/ui/Button.svelte';
  const context = () => ({
    projectId: 'qa-project',
    worktreeId: '/tmp/qa-workspace',
    paneId: 'qa-pane',
    session: 'qa-session',
  });
  setContext(EXTENSION_CONTEXT, context);
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
{#if page === 'plugins'}<PluginSettings />{:else}<ExtensionPoint
    target="component.session-surface"
    ><AgentTranscript events={events as any} /></ExtensionPoint
  ><ExtensionPoint target="settings.appearance" />{/if}
<ChatEditDialog />

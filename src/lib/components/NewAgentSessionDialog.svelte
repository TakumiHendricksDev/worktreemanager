<script lang="ts">
  import ExtensionPoint from './ExtensionPoint.svelte';
  import { extensionPart, extensionContext } from '../state/extensions.svelte';
  const publicContext = extensionContext();
  import { sessions, type AgentExperience } from '../state/sessions.svelte';
  import { plugins } from '../state/plugins.svelte';
  import { errorMessage } from '../ipc/types';
  import Button from './ui/Button.svelte';
  import Dialog from './ui/Dialog.svelte';
  import Field from './ui/Field.svelte';
  const {
    projectId,
    worktreeId,
    onclose,
  }: { projectId: string; worktreeId: string; onclose: () => void } = $props();
  const available = $derived(sessions.options.filter((p) => p.available && p.offered));
  let provider = $state('');
  let experience = $state<AgentExperience | ''>('');
  let busy = $state(false);
  let error = $state<string | null>(null);
  const chosen = $derived(provider || available[0]?.id || '');
  const selected = $derived(
    experience ||
      (plugins.state.registry.experience === 'wtm.native-cli' ? 'native' : 'chat'),
  );
  const nativeAvailable = $derived(
    ['claude', 'codex'].includes(chosen) && plugins.enabled('wtm.native-cli'),
  );
  async function open() {
    busy = true;
    try {
      const id = await sessions.openAgent(projectId, worktreeId, chosen, {
        experience: selected,
      });
      if (!id) throw new Error(sessions.error ?? 'There is no room for another session.');
      onclose();
    } catch (e) {
      error = errorMessage(e);
    } finally {
      busy = false;
    }
  }
</script>

<Dialog title="New agent session" {onclose} closeDisabled={busy}>
  {#snippet body()}
    <ExtensionPoint target="session.new"
      ><div
        class="o-stack"
        use:extensionPart={{ target: 'session.new', context: publicContext }}
      >
        <Field id="native-provider" label="Agent"
          ><select
            id="native-provider"
            class="c-input"
            value={chosen}
            onchange={(event) => (provider = event.currentTarget.value)}
            >{#each available as option}<option value={option.id}>{option.label}</option
              >{/each}</select
          ></Field
        >
        <fieldset class="c-native__choices">
          <legend>Experience</legend>
          <label class="c-native__choice"
            ><input
              type="radio"
              name="experience"
              value="native"
              checked={selected === 'native'}
              disabled={!nativeAvailable}
              onchange={() => (experience = 'native')}
            /><span
              >Native CLI with WTM integration<small
                >Vendor terminal interface with WTM context, tools and available activity.</small
              ></span
            ></label
          >
          <label class="c-native__choice"
            ><input
              type="radio"
              name="experience"
              value="chat"
              checked={selected === 'chat'}
              disabled={!plugins.enabled('wtm.chat')}
              onchange={() => (experience = 'chat')}
            /><span
              >WTM Chat<small>WTM transcript, composer and message controls.</small></span
            ></label
          >
          <label class="c-native__choice"
            ><input
              type="radio"
              name="experience"
              value="plain"
              checked={selected === 'plain'}
              disabled={!['claude', 'codex'].includes(chosen)}
              onchange={() => (experience = 'plain')}
            /><span
              >Plain terminal<small>No WTM agent context, tools or activity tracking.</small
              ></span
            ></label
          >
        </fieldset>
        <p class="c-note">
          Uses your existing provider sign-in and settings. Available integration is checked
          before launch. Existing conversations keep their interface.
        </p>
        {#if error}<p class="c-status--danger" role="alert">{error}</p>{/if}
      </div></ExtensionPoint
    >
  {/snippet}
  {#snippet footer()}<Button disabled={busy} onclick={onclose}>Cancel</Button><Button
      variant="accent"
      disabled={busy ||
        !chosen ||
        (selected === 'native' && !nativeAvailable) ||
        (selected === 'chat' && !plugins.enabled('wtm.chat')) ||
        (selected === 'plain' && !['claude', 'codex'].includes(chosen))}
      onclick={() => void open()}>{busy ? 'Opening…' : 'Open session'}</Button
    >{/snippet}
</Dialog>

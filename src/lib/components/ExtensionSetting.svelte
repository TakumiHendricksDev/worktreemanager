<script lang="ts">
  import type { Contribution, ExtensionSetting, ExtensionContext } from '../extensions';
  import { extensions } from '../state/extensions.svelte';
  import { errorMessage } from '../ipc/types';
  import Field from './ui/Field.svelte';
  import TextInput from './ui/TextInput.svelte';
  import Choice from './ui/Choice.svelte';

  const {
    setting,
    context,
    scope = '',
  }: {
    setting: Contribution<ExtensionSetting>;
    context: ExtensionContext;
    scope?: string;
  } = $props();
  let problem = $state('');
  let busy = $state(false);
  const value = $derived(
    extensions.settings(setting.plugin, setting.value.id, {
      ...context,
      worktreeId: scope,
    }),
  );
  async function save(value: unknown) {
    busy = true;
    problem = '';
    try {
      await extensions.configure(setting.plugin, {
        kind: 'setting',
        id: setting.value.id,
        worktree: scope,
        value,
      });
    } catch (e) {
      problem = errorMessage(e);
    } finally {
      busy = false;
    }
  }
</script>

<Field
  id={'extension-setting-' + setting.key}
  label={setting.value.title}
  help={setting.value.description}
  errors={[problem]}
>
  {#if setting.value.kind === 'boolean'}
    <Choice
      id={'extension-setting-' + setting.key}
      checked={value === true}
      disabled={busy}
      onchange={save}>Enabled</Choice
    >
  {:else if setting.value.kind === 'select'}
    <select
      id={'extension-setting-' + setting.key}
      class="c-input"
      value={String(value)}
      disabled={busy}
      onchange={(e) => save(e.currentTarget.value)}
    >
      {#each setting.value.options as option (option)}<option value={option}
          >{option}</option
        >{/each}
    </select>
  {:else}
    <TextInput
      id={'extension-setting-' + setting.key}
      type={setting.value.kind === 'number' ? 'number' : 'text'}
      value={String(value ?? '')}
      disabled={busy}
      onblur={(e) => {
        const value = (e.currentTarget as HTMLInputElement).value;
        void save(setting.value.kind === 'number' ? Number(value) : value);
      }}
    />
  {/if}
</Field>

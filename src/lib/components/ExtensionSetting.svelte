<script lang="ts">
  /**
   * One plugin setting, as an ordinary field: a checkbox, a select, or a text or number input.
   *
   * The value is namespaced by package and, when `scope` names one, by workspace; Rust validates
   * it against the manifest's declaration before it is stored. Shown where the manifest put it
   * (a settings section's `ExtensionPoint`, a content widget) and in the package's own details.
   *
   * A text field saves when it loses focus, and only if it changed. Every save rewrites the
   * plugin registry on disk and replaces the snapshot everything plugin-related derives from, so
   * saving on every blur did all of that for a Tab through an untouched form.
   */
  import type { Contribution, ExtensionSetting, ExtensionContext } from '../extensions';
  import { errorMessage } from '../ipc/types';
  import { extensions } from '../state/extensions.svelte';
  import Choice from './ui/Choice.svelte';
  import Field from './ui/Field.svelte';
  import TextInput from './ui/TextInput.svelte';

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
    extensions.settingValue(setting.plugin, setting.value.id, {
      ...context,
      worktreeId: scope,
    }),
  );

  async function save(next: unknown) {
    if (next === value) return;
    busy = true;
    problem = '';
    try {
      await extensions.configure(setting.plugin, {
        kind: 'setting',
        id: setting.value.id,
        worktree: scope,
        value: next,
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
        const typed = (e.currentTarget as HTMLInputElement).value;
        void save(setting.value.kind === 'number' ? Number(typed) : typed);
      }}
    />
  {/if}
</Field>

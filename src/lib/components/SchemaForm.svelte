<script lang="ts">
  /**
   * The one form renderer.
   *
   * Every field kind is handled here, switching on `field.kind`. There is deliberately no
   * per-project or per-field bespoke component: the whole point is that a project changes
   * its New Worktree dialog by editing `wtm.toml`, and that only holds if the renderer is
   * generic. `FieldKind` is a closed Rust enum, so adding a kind is a compile error on the
   * Rust side and a type error here until both handle it.
   *
   * Select options come from `field_options`, which runs the project's own command — this
   * is the "pull the options from bash" capability. Each dropdown is fetched independently
   * so the form paints immediately rather than waiting on the slowest command.
   */
  import { commands } from '../ipc/commands';
  import { errorMessage, type Field as FieldSpec } from '../ipc/types';
  import Field from './ui/Field.svelte';
  import Choice from './ui/Choice.svelte';

  const {
    projectId,
    fields,
    values,
    problems = {},
    normalized = {},
    inert = [],
    inertReason = '',
  }: {
    projectId: string;
    fields: FieldSpec[];
    values: Record<string, string>;
    /** Per-field validation messages from the backend, shown inline. */
    problems?: Record<string, string>;
    /**
     * Keys whose value no longer affects the outcome.
     *
     * Dimmed and annotated rather than hidden or disabled: the value is still *true* — it is
     * what the field says — and removing it would make the form appear to lose your input.
     * Disabling would be worse still, because clearing the condition must put the field back
     * exactly as you left it.
     */
    inert?: string[];
    /** Shown beneath each inert field. Says *why*, since the field looks fine otherwise. */
    inertReason?: string;
    /**
     * The effective value after the field's `normalize` template ran.
     *
     * Displayed next to the input when it differs, so a user typing `1234` can see it became
     * `ACME-1234` *before* pressing Create rather than being surprised by the branch name.
     */
    normalized?: Record<string, string>;
  } = $props();

  type OptionState = { loading: boolean; values: string[]; error: string | null };

  const options = $state<Record<string, OptionState>>({});

  // Seed defaults once, so a re-render does not clobber what the user typed.
  $effect(() => {
    for (const field of fields) {
      if (values[field.key] === undefined) {
        values[field.key] = field.default ?? (field.kind === 'bool' ? 'false' : '');
      }
    }
  });

  $effect(() => {
    for (const field of fields) {
      if (!field.hasDynamicOptions || options[field.key]) continue;

      options[field.key] = { loading: true, values: [], error: null };
      void commands
        .fieldOptions(projectId, field.key)
        .then((list) => {
          options[field.key] = { loading: false, values: list, error: null };
        })
        .catch((e) => {
          // A failed options command must not block the form — the field stays typeable
          // when `allowCustom` is set, and the reason is shown either way.
          options[field.key] = { loading: false, values: [], error: errorMessage(e) };
        });
    }
  });

  function optionsFor(field: FieldSpec): string[] {
    return field.hasDynamicOptions ? (options[field.key]?.values ?? []) : field.options;
  }

  function selectedOf(key: string): string[] {
    return (values[key] ?? '')
      .split(',')
      .map((part) => part.trim())
      .filter(Boolean);
  }

  function toggleMulti(key: string, option: string, on: boolean): void {
    const next = new Set(selectedOf(key));
    if (on) next.add(option);
    else next.delete(option);
    values[key] = [...next].join(',');
  }

  const inertKeys = $derived(new Set(inert));
</script>

<div class="o-stack o-stack--loose">
  {#each fields as field (field.key)}
    <Field
      id={`f-${field.key}`}
      label={field.label}
      required={field.required}
      inert={inertKeys.has(field.key)}
      {inertReason}
      note={!inertKeys.has(field.key) &&
      normalized[field.key] &&
      normalized[field.key] !== values[field.key]
        ? normalized[field.key]
        : null}
      errors={[
        problems[field.key],
        options[field.key]?.error && `Could not load options: ${options[field.key]?.error}`,
      ]}
      help={field.kind === 'bool' ? null : field.help}
    >
      {#if field.kind === 'bool'}
        <Choice
          id={`f-${field.key}`}
          checked={values[field.key] === 'true'}
          onchange={(on) => (values[field.key] = on ? 'true' : 'false')}
        >
          {field.help ?? ''}
        </Choice>
      {:else if field.kind === 'multiselect'}
        <div class="o-stack">
          {#each optionsFor(field) as option (option)}
            <Choice
              checked={selectedOf(field.key).includes(option)}
              onchange={(on) => toggleMulti(field.key, option, on)}
            >
              {option}
            </Choice>
          {/each}
          {#if field.allowCustom}
            <input
              id={`f-${field.key}`}
              class="c-input"
              type="text"
              placeholder={field.placeholder ?? 'Add extra values, comma-separated'}
              value={selectedOf(field.key)
                .filter((value) => !optionsFor(field).includes(value))
                .join(', ')}
              oninput={(e) => {
                const extras = (e.currentTarget as HTMLInputElement).value
                  .split(',')
                  .map((part) => part.trim())
                  .filter(Boolean);
                const known = selectedOf(field.key).filter((value) =>
                  optionsFor(field).includes(value),
                );
                values[field.key] = [...known, ...extras].join(',');
              }}
            />
          {/if}
        </div>
      {:else if field.kind === 'select'}
        {#if field.allowCustom}
          <!-- One searchable combobox: typing filters the browser's suggestions and is also a
               valid custom ref. The old select-plus-input made the same value look like two fields. -->
          <input
            id={`f-${field.key}`}
            class="c-input"
            type="text"
            list={`options-${field.key}`}
            placeholder={options[field.key]?.loading
              ? 'Loading refs…'
              : (field.placeholder ?? 'Search or type a ref')}
            bind:value={values[field.key]}
          />
          <datalist id={`options-${field.key}`}>
            {#each optionsFor(field) as option (option)}
              <option value={option}></option>
            {/each}
          </datalist>
        {:else}
          <select id={`f-${field.key}`} class="c-select" bind:value={values[field.key]}>
            {#if options[field.key]?.loading}
              <option value={values[field.key]}>Loading…</option>
            {:else}
              {#if !optionsFor(field).includes(values[field.key] ?? '')}
                <!-- Keep a default or custom value selectable even when the command's
                     output does not contain it. -->
                <option value={values[field.key]}>{values[field.key] || '—'}</option>
              {/if}
              {#each optionsFor(field) as option (option)}
                <option value={option}>{option}</option>
              {/each}
            {/if}
          </select>
        {/if}
      {:else if field.kind === 'multiline'}
        <textarea
          id={`f-${field.key}`}
          class="c-textarea"
          rows="3"
          bind:value={values[field.key]}></textarea>
      {:else if field.kind === 'number'}
        <input
          id={`f-${field.key}`}
          class="c-input"
          type="number"
          value={values[field.key]}
          oninput={(e) => (values[field.key] = (e.currentTarget as HTMLInputElement).value)}
        />
      {:else}
        <input
          id={`f-${field.key}`}
          class="c-input"
          type="text"
          placeholder={field.placeholder ?? ''}
          bind:value={values[field.key]}
        />
      {/if}
    </Field>
  {/each}
</div>

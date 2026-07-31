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
        <label class="c-choice">
          <input
            id={`f-${field.key}`}
            type="checkbox"
            checked={values[field.key] === 'true'}
            onchange={(e) =>
              (values[field.key] = (e.currentTarget as HTMLInputElement).checked
                ? 'true'
                : 'false')}
          />
          <span class="c-choice__hint">{field.help ?? ''}</span>
        </label>
      {:else if field.kind === 'select' || field.kind === 'multiselect'}
        <div class="c-schema-form__select-row">
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
          {#if field.allowCustom}
            <input
              class="c-input c-schema-form__custom"
              type="text"
              placeholder="or type a ref"
              value={values[field.key] ?? ''}
              oninput={(e) =>
                (values[field.key] = (e.currentTarget as HTMLInputElement).value)}
            />
          {/if}
        </div>
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
          bind:value={values[field.key]}
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

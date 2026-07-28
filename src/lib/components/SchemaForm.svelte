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
  import { errorMessage, type Field } from '../ipc/types';

  const {
    projectId,
    fields,
    values,
    problems = {},
    normalized = {},
  }: {
    projectId: string;
    fields: Field[];
    values: Record<string, string>;
    /** Per-field validation messages from the backend, shown inline. */
    problems?: Record<string, string>;
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

  function optionsFor(field: Field): string[] {
    return field.hasDynamicOptions ? (options[field.key]?.values ?? []) : field.options;
  }
</script>

<div class="form">
  {#each fields as field (field.key)}
    <div class="field">
      <label for={`f-${field.key}`}>
        {field.label}
        {#if field.required}<span class="req" aria-label="required">*</span>{/if}
      </label>

      {#if field.kind === 'bool'}
        <label class="check">
          <input
            id={`f-${field.key}`}
            type="checkbox"
            checked={values[field.key] === 'true'}
            onchange={(e) =>
              (values[field.key] = (e.currentTarget as HTMLInputElement).checked
                ? 'true'
                : 'false')}
          />
          <span class="checklabel">{field.help ?? ''}</span>
        </label>
      {:else if field.kind === 'select' || field.kind === 'multiselect'}
        <div class="selectrow">
          <select id={`f-${field.key}`} bind:value={values[field.key]}>
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
              class="custom"
              type="text"
              placeholder="or type a ref"
              value={values[field.key] ?? ''}
              oninput={(e) =>
                (values[field.key] = (e.currentTarget as HTMLInputElement).value)}
            />
          {/if}
        </div>
      {:else if field.kind === 'multiline'}
        <textarea id={`f-${field.key}`} rows="3" bind:value={values[field.key]}></textarea>
      {:else if field.kind === 'number'}
        <input id={`f-${field.key}`} type="number" bind:value={values[field.key]} />
      {:else}
        <input
          id={`f-${field.key}`}
          type="text"
          placeholder={field.placeholder ?? ''}
          bind:value={values[field.key]}
        />
      {/if}

      {#if normalized[field.key] && normalized[field.key] !== values[field.key]}
        <p class="normalized">
          → <code>{normalized[field.key]}</code>
        </p>
      {/if}
      {#if problems[field.key]}
        <p class="help error">{problems[field.key]}</p>
      {/if}
      {#if field.help && field.kind !== 'bool'}
        <p class="help">{field.help}</p>
      {/if}
      {#if options[field.key]?.error}
        <p class="help error">
          Could not load options: {options[field.key]?.error}
        </p>
      {/if}
    </div>
  {/each}
</div>

<style>
  .form {
    display: flex;
    flex-direction: column;
    gap: var(--sp-4);
  }

  .field {
    display: flex;
    flex-direction: column;
    gap: var(--sp-1);
  }

  label {
    font-size: var(--step--1);
    font-weight: 500;
  }

  .req {
    color: var(--accent);
    margin-left: 2px;
  }

  input[type='text'],
  input[type='number'],
  select,
  textarea {
    width: 100%;
    padding: 6px 9px;
    border: 1px solid var(--border);
    border-radius: var(--r-md);
    background: var(--bg-input);
    font-size: var(--step-0);
  }

  input[type='text']:focus,
  select:focus,
  textarea:focus {
    border-color: var(--border-focus);
  }

  textarea {
    font-family: var(--font-ui);
    resize: vertical;
  }

  .selectrow {
    display: grid;
    grid-template-columns: 1fr auto;
    gap: var(--sp-2);
  }

  .custom {
    max-width: 180px;
  }

  .check {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    font-weight: 400;
  }

  .checklabel {
    color: var(--fg-muted);
    font-size: var(--step--1);
  }

  .help {
    color: var(--fg-muted);
    font-size: var(--step--2);
    line-height: 1.5;
  }

  .help.error {
    color: var(--danger);
  }

  .normalized {
    font-size: var(--step--2);
    color: var(--accent);
  }
</style>

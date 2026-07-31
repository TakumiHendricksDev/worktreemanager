<script lang="ts">
  /**
   * Add a repository.
   *
   * This exists because the first version used `window.prompt()`, which a Tauri webview does
   * not implement — it returns `null` immediately, so the button appeared to do nothing at
   * all. `window.alert` and `window.confirm` are the same story. Nothing in this app may rely
   * on them.
   *
   * Both a text field and a Browse… button, deliberately.
   *
   * This used to be typed-only, on the grounds that the dialog plugin is another dependency and
   * another capability and a developer already knows their own paths. That was wrong about who
   * uses it: knowing the path and having it *on the clipboard* are different things, and typing
   * one out is the slowest way to do this. So the picker is here now, and `dialog:allow-open` is
   * the one capability this app grants beyond its own commands — it returns a single
   * user-chosen path and nothing else. The reasoning that survives is the other half: the field
   * stays, because pasting a path you already have beats hunting for it in a file browser.
   *
   * Picking does not submit. Registration resolves whatever it is given to the repository root,
   * so choosing a subdirectory registers somewhere other than what you clicked — showing the
   * path first is the difference between that being a feature and being a surprise.
   */
  import { open } from '@tauri-apps/plugin-dialog';

  import { errorMessage } from '../ipc/types';
  import Button from './ui/Button.svelte';
  import Dialog from './ui/Dialog.svelte';
  import Field from './ui/Field.svelte';
  import TextInput from './ui/TextInput.svelte';
  import { workspace } from '../state/workspace.svelte';

  const { onclose }: { onclose: () => void } = $props();

  let path = $state('');
  let busy = $state(false);
  let error = $state<string | null>(null);
  let input = $state<HTMLInputElement | null>(null);

  $effect(() => {
    input?.focus();
  });

  async function browse() {
    error = null;
    try {
      const picked = await open({
        directory: true,
        multiple: false,
        title: 'Choose a git repository',
      });
      // `null` is a cancelled dialog, which is not a failure and must not say anything.
      if (typeof picked === 'string') path = picked;
    } catch (e) {
      // Surfaced rather than swallowed: the only realistic cause is a missing capability, and
      // a Browse button that silently does nothing is the bug this file's header opens with.
      error = errorMessage(e);
    } finally {
      input?.focus();
    }
  }

  async function onsubmit(event: Event) {
    event.preventDefault();
    const trimmed = path.trim().replace(/\/+$/, '');
    if (!trimmed) return;

    busy = true;
    error = null;
    try {
      // `~` is expanded in Rust, which actually knows HOME.
      await workspace.addProject(trimmed);
      onclose();
    } catch (e) {
      error = errorMessage(e);
    } finally {
      busy = false;
    }
  }
</script>

<Dialog title="Add a repository" {onclose} {onsubmit} closeDisabled={busy}>
  {#snippet body()}
    <Field
      id="repo-path"
      label="Path to a git repository"
      help="Any path inside the repository works — wtm resolves it to the root. Nothing is written to the repository itself."
      errors={[error]}
    >
      <div class="c-add-project__row">
        <TextInput
          id="repo-path"
          bind:value={path}
          bind:element={input}
          mono
          placeholder="~/Sites/your-repo"
        />
        <Button variant="neutral" onclick={browse} disabled={busy}>Browse…</Button>
      </div>
    </Field>
  {/snippet}

  {#snippet footer()}
    <Button variant="neutral" onclick={onclose}>Cancel</Button>
    <Button type="submit" variant="accent" disabled={busy || path.trim() === ''}>
      {busy ? 'Adding…' : 'Add'}
    </Button>
  {/snippet}
</Dialog>

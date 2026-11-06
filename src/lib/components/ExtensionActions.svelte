<script lang="ts">
  import type { ExtensionContext } from '../extensions';
  import { extensions, extensionContext } from '../state/extensions.svelte';
  import { item, popUp, under } from '../native-menu';
  import { errorMessage } from '../ipc/types';
  import Button from './ui/Button.svelte';
  import Banner from './ui/Banner.svelte';

  const inherited = extensionContext();
  const { target, context }: { target: string; context?: ExtensionContext } = $props();
  const current = $derived(context ?? inherited());
  const actions = $derived(
    extensions
      .contributions('actions', current)
      .filter((c) => c.value.target === target && !c.value.replaces),
  );
  let problem = $state('');
  function open(event: MouseEvent) {
    problem = '';
    void popUp(
      actions.map((a) =>
        item(a.value.title, () => {
          void extensions.action(a, current).catch((e) => {
            problem = errorMessage(e);
          });
        }),
      ),
      under(event.currentTarget as HTMLElement),
    );
  }
</script>

{#if actions.length}<Button
    size="sm"
    variant="quiet"
    ariaLabel="Plugin actions"
    onclick={open}>Plugin actions</Button
  >{/if}
{#if problem}<Banner variant="error">{problem}</Banner>{/if}

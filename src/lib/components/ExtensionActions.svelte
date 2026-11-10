<script lang="ts">
  /**
   * A surface's Plugin actions button: one native menu of every enabled action aimed at it.
   *
   * Placed by the feature among its own controls (`workspace.actions` in the worktree bar,
   * `panel.actions` in a pane's header) and by `ExtensionPoint` for surfaces that take actions as
   * well as content. Renders nothing when no action applies here, so an untargeted toolbar keeps
   * its shape.
   *
   * A menu rather than one button per action: a pane header has no room for a plugin's whole
   * catalogue, and a native menu brings keyboard navigation and type-ahead for free.
   */
  import type { ExtensionContext, Surface } from '../extensions';
  import { errorMessage } from '../ipc/types';
  import { item, popUp, under } from '../native-menu';
  import { extensions, extensionContext } from '../state/extensions.svelte';
  import Banner from './ui/Banner.svelte';
  import Button from './ui/Button.svelte';

  const {
    target,
    context,
  }: { target: Surface; context?: (() => ExtensionContext) | null } = $props();

  const inherited = extensionContext();
  const source = $derived(inherited && context !== null ? (context ?? inherited) : null);
  const current = $derived(source && extensions.targets(target) ? source() : null);
  const actions = $derived(current ? extensions.actions(target, current) : []);
  let problem = $state('');

  function open(event: MouseEvent) {
    const here = current;
    if (!here) return;
    problem = '';
    void popUp(
      actions.map((a) =>
        item(a.value.title, () => {
          void extensions.perform(a, here).catch((e) => {
            problem = errorMessage(e);
          });
        }),
      ),
      under(event.currentTarget as HTMLElement),
    );
  }
</script>

{#if actions.length}<Button size="sm" variant="quiet" ariaHaspopup="menu" onclick={open}
    >Plugin actions</Button
  >{/if}
{#if problem}<Banner variant="error">{problem}</Banner>{/if}

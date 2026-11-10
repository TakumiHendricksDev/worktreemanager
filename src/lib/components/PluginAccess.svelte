<script lang="ts">
  /**
   * What a package may run and ask wtm to do: its programs, the host operations it declared, and
   * the events it receives. Read-only, in the review before install and in an installed package's
   * details, so the two show the same facts the same way.
   *
   * Every argv is written by `argvText` and labelled by kind. The runtime was joined with spaces
   * and the rest were JSON, unlabelled, so the same package read two ways in one dialog and a
   * runtime argument containing a space could not be told from two arguments.
   *
   * `executable` comes from the caller rather than from `executes` here, because a review has the
   * host's own answer (`PluginReview.executable`) and should show that, not a copy of the rule.
   */
  import {
    argvText,
    eventReason,
    operationReason,
    packageParts,
    PACKAGE_PART_LABELS,
    type PluginManifest,
  } from '../plugins';
  import Banner from './ui/Banner.svelte';

  const {
    manifest,
    executable,
    level = 5,
  }: {
    manifest: PluginManifest;
    executable: boolean;
    /** The heading level of the subsections, one below whatever this sits under. */
    level?: 4 | 5;
  } = $props();

  const programs = $derived([
    ...(manifest.extensions.runtime.length
      ? [{ label: 'Runtime', title: null, argv: manifest.extensions.runtime }]
      : []),
    ...packageParts(manifest).flatMap((part) =>
      part.argv
        ? [{ label: PACKAGE_PART_LABELS[part.kind], title: part.title, argv: part.argv }]
        : [],
    ),
  ]);
  const heading = $derived(`h${level}`);
</script>

<div class="o-stack">
  {#if executable}
    <Banner variant="warn"
      >This plugin acts as native code. It can run programs or ask wtm to act for you, as
      your user, with access to your files and the network. It is not sandboxed. Install
      only code you trust.</Banner
    >
  {/if}
  {#if programs.length}
    <svelte:element this={heading} class="c-section-heading">Runs</svelte:element>
    <ul class="o-plain-list o-stack">
      {#each programs as program}
        <li class="o-stack o-stack--tight">
          <span
            ><strong>{program.label}</strong
            >{#if program.title}{` · ${program.title}`}{/if}</span
          >
          <pre class="c-plugins__code">{argvText(program.argv)}</pre>
        </li>
      {/each}
    </ul>
  {/if}
  {#if manifest.extensions.access.length}
    <svelte:element this={heading} class="c-section-heading">Asks wtm to</svelte:element>
    <ul class="o-plain-list o-stack">
      {#each manifest.extensions.access as operation (operation)}
        <li class="o-stack o-stack--tight">
          <code>{operation}</code>
          <span class="c-note">{operationReason(operation)}</span>
        </li>
      {/each}
    </ul>
  {/if}
  {#if manifest.extensions.events.length}
    <svelte:element this={heading} class="c-section-heading">Is told about</svelte:element>
    <ul class="o-plain-list o-stack">
      {#each manifest.extensions.events as event (event)}
        <li class="o-stack o-stack--tight">
          <code>{event}</code>
          <span class="c-note">{eventReason(event)}</span>
        </li>
      {/each}
    </ul>
  {/if}
</div>

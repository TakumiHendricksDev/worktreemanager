<script lang="ts">
  /**
   * The window's top strip.
   *
   * Doubles as the drag region, because `titleBarStyle: "Overlay"` removes the native
   * one — without `data-tauri-drag-region` the window could not be moved at all. The
   * left gutter is reserved for the traffic lights, which are positioned by
   * `trafficLightPosition` in tauri.conf.json and would otherwise sit on top of content.
   */
  import { theme, type ThemeChoice } from '../state/theme.svelte';

  const { title, subtitle }: { title: string; subtitle?: string } = $props();

  const icons: Record<ThemeChoice, string> = {
    system: '◐',
    light: '☀',
    dark: '☾',
  };

  const labels: Record<ThemeChoice, string> = {
    system: 'Theme: following system',
    light: 'Theme: light',
    dark: 'Theme: dark',
  };
</script>

<header class="titlebar" data-tauri-drag-region>
  <!-- Reserves space for the macOS traffic lights. -->
  <div class="gutter" data-tauri-drag-region></div>

  <div class="identity" data-tauri-drag-region>
    <span class="title">{title}</span>
    {#if subtitle}
      <span class="sep" aria-hidden="true">/</span>
      <span class="subtitle">{subtitle}</span>
    {/if}
  </div>

  <div class="actions">
    <button
      class="theme"
      onclick={() => theme.cycle()}
      title={labels[theme.choice]}
      aria-label={labels[theme.choice]}
    >
      <span aria-hidden="true">{icons[theme.choice]}</span>
    </button>
  </div>
</header>

<style>
  .titlebar {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    height: var(--titlebar-h);
    padding-right: var(--sp-3);
    border-bottom: 1px solid var(--border);
    /* The drag region must not select text as the user moves the window. */
    user-select: none;
    -webkit-user-select: none;
    flex: 0 0 auto;
  }

  /* Space for the traffic lights, positioned natively at x: 16, y: 20. */
  .gutter {
    width: 76px;
    height: 100%;
    flex: 0 0 auto;
  }

  .identity {
    display: flex;
    align-items: baseline;
    gap: var(--sp-2);
    min-width: 0;
    flex: 1 1 auto;
  }

  .title {
    font-weight: 600;
    font-size: var(--step-0);
    white-space: nowrap;
  }

  .sep {
    color: var(--fg-subtle);
  }

  .subtitle {
    color: var(--fg-muted);
    font-size: var(--step--1);
    font-family: var(--font-mono);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .actions {
    display: flex;
    align-items: center;
    gap: var(--sp-1);
    flex: 0 0 auto;
  }

  .theme {
    width: 28px;
    height: 28px;
    display: grid;
    place-items: center;
    border-radius: var(--r-md);
    color: var(--fg-muted);
    font-size: var(--step-1);
    transition:
      background var(--dur-fast) var(--ease),
      color var(--dur-fast) var(--ease);
  }

  .theme:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }
</style>

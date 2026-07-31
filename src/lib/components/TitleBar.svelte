<script lang="ts">
  /**
   * The window's top strip: project switcher on the left, window controls on the right.
   *
   * Doubles as the drag region, because `titleBarStyle: "Overlay"` removes the native
   * one — without `data-tauri-drag-region` the window could not be moved at all. The
   * left gutter is reserved for the traffic lights, which are positioned by
   * `trafficLightPosition` in tauri.conf.json and would otherwise sit on top of content.
   *
   * # Why the project switcher lives here
   *
   * It started in the sidebar, above the worktree list. Two problems with that: the strip
   * beside the traffic lights was already spelling out the project name and root, so the
   * same fact appeared twice; and the sidebar's top is where a search field belongs, since
   * that is what it filters. Moving the switcher up here removes the duplication and leaves
   * the sidebar to do one thing.
   *
   * A native `<select>` rather than a hand-rolled popover: keyboard navigation, type-ahead,
   * click-outside and Escape all come free and behave the way macOS menus are expected to.
   * It is styled to read as a title-bar button, not a form control.
   */
  import { theme, type ThemeChoice } from '../state/theme.svelte';
  import { workspace } from '../state/workspace.svelte';

  const { onaddproject }: { onaddproject: () => void } = $props();

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

  async function onProjectChange(event: Event) {
    const select = event.currentTarget as HTMLSelectElement;
    if (select.value === '__add__') {
      // Re-select the current project so the picker does not stay on the sentinel.
      select.value = workspace.activeProjectId ?? '';
      onaddproject();
      return;
    }
    await workspace.selectProject(select.value);
  }
</script>

<header class="titlebar" data-tauri-drag-region>
  <!-- Reserves space for the macOS traffic lights; a plain inset on Linux, where the
       window manager draws the controls outside the webview. -->
  <div class="gutter" data-tauri-drag-region></div>

  <!--
    Drag region on the container, not just the text: the path may be short, and the empty
    space beside it still has to move the window. Tauri only starts a drag when the event's
    own target carries the attribute, so the select and its caret are unaffected.
  -->
  <div class="identity" data-tauri-drag-region>
    <div class="picker">
      <label class="u-visually-hidden" for="project-picker">Project</label>
      <!--
        The visible label is a span, and the real `<select>` is stretched invisibly over it.
        A bare select sizes itself to its *widest option* — here `Add a repository…` — which
        strands the caret a couple of hundred pixels from a short project name. Rendering the
        label separately sizes the control to what is actually selected.

        The span is `aria-hidden` because the select underneath is the real control and
        already carries this text; without it a screen reader reads the name twice.
      -->
      <span class="label" aria-hidden="true">
        <span class="name">{workspace.activeProject?.name ?? 'No projects yet'}</span>
        <span class="caret">⌄</span>
      </span>
      <select
        id="project-picker"
        value={workspace.activeProjectId ?? ''}
        onchange={onProjectChange}
        disabled={workspace.projects.length === 0}
      >
        {#if workspace.projects.length === 0}
          <option value="">No projects yet</option>
        {/if}
        {#each workspace.projects as project (project.id)}
          <option value={project.id}>
            {project.name}{project.usable ? '' : '  ⚠'}
          </option>
        {/each}
        <option value="__add__">Add a repository…</option>
      </select>
    </div>

    {#if workspace.activeProject}
      <!--
        The repository root, for orientation. Not `title`d: it is already the full string,
        and a tooltip repeating what is on screen is noise.
      -->
      <span class="root" data-tauri-drag-region>{workspace.activeProject.root}</span>
    {/if}
  </div>

  <div class="actions">
    <button
      class="icon"
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
    gap: var(--sp-2);
    height: var(--titlebar-h);
    padding-right: var(--sp-2);
    border-bottom: 1px solid var(--border);
    /* The drag region must not select text as the user moves the window. */
    user-select: none;
    -webkit-user-select: none;
    flex: 0 0 auto;
  }

  /*
    On macOS, space for the traffic lights — the arithmetic behind the 76px lives with
    the token in app.css. On Linux the window manager draws the controls on the right,
    outside the webview entirely, so this collapses to an ordinary leading inset that
    stays draggable.
  */
  .gutter {
    width: var(--titlebar-lead);
    height: 100%;
    flex: 0 0 auto;
  }

  .identity {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    min-width: 0;
    flex: 1 1 auto;
  }

  .picker {
    position: relative;
    flex: 0 0 auto;
    display: inline-flex;
    align-items: center;
    min-width: 0;
  }

  /* Reads as a title-bar button: no field chrome until you interact with it. */
  .label {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    height: 24px;
    max-width: 220px;
    padding: 0 7px;
    border: 1px solid transparent;
    border-radius: var(--r-md);
    font-size: var(--step-0);
    font-weight: 600;
    white-space: nowrap;
    transition:
      background var(--dur-fast) var(--ease),
      border-color var(--dur-fast) var(--ease);
  }

  .name {
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .caret {
    flex: 0 0 auto;
    color: var(--fg-subtle);
    font-size: var(--step--1);
    line-height: 1;
  }

  /*
    The real control, stretched over the label and invisible. `opacity: 0` rather than
    `visibility: hidden`: it has to stay hit-testable and focusable, and the native pop-up
    menu still anchors to its box.
  */
  select {
    position: absolute;
    inset: 0;
    width: 100%;
    appearance: none;
    opacity: 0;
    /* Safari renders a caret on a disabled select without this, which would show through. */
    background: transparent;
  }

  .picker:hover .label {
    background: var(--bg-hover);
    border-color: var(--border);
  }

  /* The select carries focus but cannot show a ring while it is invisible, so the label
     wears it instead. */
  .picker:focus-within .label {
    border-color: var(--border-focus);
  }

  .picker:has(select:disabled) .label {
    color: var(--fg-muted);
  }

  .picker:has(select:disabled):hover .label {
    background: transparent;
    border-color: transparent;
  }

  .root {
    color: var(--fg-subtle);
    font-size: var(--step--2);
    font-family: var(--font-mono);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    min-width: 0;
  }

  .actions {
    display: flex;
    align-items: center;
    gap: var(--sp-1);
    flex: 0 0 auto;
  }

  .icon {
    width: 24px;
    height: 24px;
    display: grid;
    place-items: center;
    border-radius: var(--r-md);
    color: var(--fg-muted);
    font-size: var(--step-0);
    transition:
      background var(--dur-fast) var(--ease),
      color var(--dur-fast) var(--ease);
  }

  .icon:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }
</style>

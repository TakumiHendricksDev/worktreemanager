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
  import { sessions } from '../state/sessions.svelte';
  import { theme, type ThemeChoice } from '../state/theme.svelte';
  import { workspace } from '../state/workspace.svelte';
  import Button from './ui/Button.svelte';
  import Icon from './ui/Icon.svelte';
  import type { IconName } from './ui/icons';

  const {
    sidebarCollapsed,
    onaddproject,
    onsettings,
    ontogglesidebar,
  }: {
    sidebarCollapsed: boolean;
    onaddproject: () => void;
    onsettings: () => void;
    ontogglesidebar: () => void;
  } = $props();

  const themeIcons: Record<ThemeChoice, IconName> = {
    system: 'theme-system',
    light: 'theme-light',
    dark: 'theme-dark',
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

<header class="c-titlebar" data-tauri-drag-region>
  <!-- Reserves space for the macOS traffic lights; a plain inset on Linux, where the
       window manager draws the controls outside the webview. -->
  <div class="c-titlebar__gutter" data-tauri-drag-region></div>

  <Button
    variant="quiet"
    icon="md"
    onclick={ontogglesidebar}
    title={sidebarCollapsed ? 'Show worktree sidebar' : 'Hide worktree sidebar'}
    ariaLabel={sidebarCollapsed ? 'Show worktree sidebar' : 'Hide worktree sidebar'}
    ariaExpanded={!sidebarCollapsed}
    ariaControls="worktree-sidebar"
  >
    <Icon name={sidebarCollapsed ? 'chevron-right' : 'chevron-left'} />
  </Button>

  <!--
    Drag region on the container, not just the text: the path may be short, and the empty
    space beside it still has to move the window. Tauri only starts a drag when the event's
    own target carries the attribute, so the select and its caret are unaffected.
  -->
  <div class="c-titlebar__identity" data-tauri-drag-region>
    <div class="c-titlebar__picker o-overlay-select">
      <label class="u-visually-hidden" for="project-picker">Project</label>
      <!--
        The visible label is a span, and the real `<select>` is stretched invisibly over it.
        A bare select sizes itself to its *widest option* — here `Add a repository…` — which
        strands the caret a couple of hundred pixels from a short project name. Rendering the
        label separately sizes the control to what is actually selected.

        The span is `aria-hidden` because the select underneath is the real control and
        already carries this text; without it a screen reader reads the name twice.
      -->
      <span class="c-titlebar__project" aria-hidden="true">
        <span class="c-titlebar__name">
          {workspace.activeProject?.name ?? 'No projects yet'}
        </span>
        <Icon name="chevron-down" size={12} />
      </span>
      <select
        id="project-picker"
        class="o-overlay-select__native"
        value={workspace.activeProjectId ?? ''}
        onchange={onProjectChange}
        disabled={workspace.projects.length === 0}
      >
        {#if workspace.projects.length === 0}
          <option value="">No projects yet</option>
        {/if}
        <!--
          A text glyph rather than a dot component, and `icons.ts` states the rule: an `<option>` may
          contain text and nothing else, so an SVG cannot go here at all. The `⚠` beside it is the
          same exemption, already taken for the same reason.

          This one glyph matters more than it looks. Panes carry a `projectId`, but the sidebar lists
          only the *active* project's worktrees — so the row dots alone still leave a session blocked
          in another project completely invisible. This and the dock badge are what close that.
        -->
        {#each workspace.projects as project (project.id)}
          <option value={project.id}>
            {project.name}{project.usable ? '' : '  ⚠'}{sessions.wantsAttentionIn(
              project.id,
            )
              ? '  ●'
              : ''}
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
      <span class="c-titlebar__root" data-tauri-drag-region>
        {workspace.activeProject.root}
      </span>
    {/if}
  </div>

  <div class="c-titlebar__actions">
    <!--
      Kept beside Settings rather than absorbed into it. Cycling light and dark is the one
      appearance change people make several times a day — chasing the sun, or a screen share
      — and putting it two clicks deep to avoid having two buttons would be the wrong trade.
      The same control appears in Settings as an explicit three-way choice, which is what the
      cycle cannot be in 24 pixels.
    -->
    <Button
      variant="quiet"
      icon="md"
      onclick={() => theme.cycle()}
      title={labels[theme.choice]}
      ariaLabel={labels[theme.choice]}
    >
      <Icon name={themeIcons[theme.choice]} />
    </Button>

    <!--
      On macOS this duplicates the app menu's Settings… item, deliberately. The menu is the
      convention and carries ⌘,; the button is how anyone finds it without knowing that. On
      Linux there is no app menu at all, so it is the only affordance.
    -->
    <Button
      variant="quiet"
      icon="md"
      onclick={onsettings}
      title="Settings"
      ariaLabel="Settings"
    >
      <Icon name="settings" />
    </Button>
  </div>
</header>

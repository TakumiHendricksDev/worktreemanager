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
   * A native menu rather than a hand-rolled popover: keyboard navigation, type-ahead,
   * click-outside and Escape all come free and behave the way macOS menus are expected to. It was
   * a `<select>` until it was clear that macOS opens one on the selected row, which pushed every
   * project above the active one off the top of the screen — see `native-menu.ts`. It is styled to
   * read as a title-bar button, not a form control.
   *
   * # Why the sidebar toggle lives here too
   *
   * It used to be two controls: a hide chevron in the sidebar's own header, and, once it was
   * hidden, a tab pinned over the top-left corner of the detail pane to bring it back. That tab
   * sat on top of whatever the pane put in its corner — the favourite star, in the worktree bar —
   * and every view the pane can show would have had to leave room for it. Beside the traffic
   * lights is where Finder, Mail and Xcode put this button, it is in the same place in both
   * states, and nothing is underneath it.
   */
  import { sessions } from '../state/sessions.svelte';
  import { theme, type ThemeChoice } from '../state/theme.svelte';
  import { workspace } from '../state/workspace.svelte';
  import { choice, item, popUp, separator, under, type MenuEntry } from '../native-menu';
  import Button from './ui/Button.svelte';
  import Icon from './ui/Icon.svelte';
  import type { IconName } from './ui/icons';

  const {
    sidebarCollapsed,
    ontogglesidebar,
    onaddproject,
    onremoveproject,
    onsettings,
  }: {
    sidebarCollapsed: boolean;
    ontogglesidebar: () => void;
    onaddproject: () => void;
    onremoveproject: () => void;
    onsettings: () => void;
  } = $props();

  /**
   * macOS's own chord for View › Show Sidebar. `App.svelte`'s keydown handler is what binds it.
   *
   * None on Linux. There is no convention there to follow, and the free chords all belong to a
   * shell: Ctrl-B, the one VS Code uses, is readline's backward-char.
   */
  const SIDEBAR_SHORTCUT =
    document.documentElement.dataset.platform === 'linux' ? null : '⌃⌘S';

  const sidebarLabel = $derived(
    `${sidebarCollapsed ? 'Show' : 'Hide'} worktree sidebar${SIDEBAR_SHORTCUT ? ` (${SIDEBAR_SHORTCUT})` : ''}`,
  );

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

  function projectMenu(event: MouseEvent) {
    const active = workspace.activeProject;
    const entries: MenuEntry[] = [
      /*
        Glyphs rather than a dot component, because a menu row is text and nothing else.

        The `●` matters more than it looks. Panes carry a `projectId`, but the sidebar lists only
        the *active* project's worktrees — so the row dots alone still leave a session blocked in
        another project completely invisible. This and the dock badge are what close that.
      */
      ...workspace.projects.map((project) =>
        choice(
          project.name +
            (project.usable ? '' : '  ⚠') +
            (sessions.wantsAttentionIn(project.id) ? '  ●' : ''),
          project.id === active?.id,
          () => {
            if (project.id !== active?.id) void workspace.selectProject(project.id);
          },
        ),
      ),
      /*
        The actions past a separator, so they do not read as two more repositories.

        Remove sits beside Add because this is where a person looks for it: the list of
        repositories is the thing being edited. It used to exist only on the error card of a project
        that failed to load, so a working repository could not be removed at all. It names the active
        project rather than offering a submenu of all of them, because removing the one on screen is
        a choice made while looking at it.
      */
      separator,
      item('Add a repository…', onaddproject),
      ...(active ? [item(`Remove “${active.name}” from wtm…`, onremoveproject)] : []),
    ];
    void popUp(entries, under(event.currentTarget as Element));
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
    title={sidebarLabel}
    ariaLabel={sidebarLabel}
    ariaExpanded={!sidebarCollapsed}
    ariaControls="worktree-sidebar"
  >
    <Icon name="sidebar" />
  </Button>

  <!--
    Drag region on the container, not just the text: the path may be short, and the empty
    space beside it still has to move the window. Tauri only starts a drag when the event's
    own target carries the attribute, so the picker and its caret are unaffected.
  -->
  <div class="c-titlebar__identity" data-tauri-drag-region>
    <button
      class="c-titlebar__project"
      aria-label="Project: {workspace.activeProject?.name ?? 'none'}"
      aria-haspopup="menu"
      onclick={projectMenu}
      disabled={workspace.projects.length === 0}
    >
      <span class="c-titlebar__name">
        {workspace.activeProject?.name ?? 'No projects yet'}
      </span>
      <Icon name="chevron-down" size={12} />
    </button>

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

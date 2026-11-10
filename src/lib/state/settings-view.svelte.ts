/**
 * Whether Settings is open, and at which section.
 *
 * A store rather than App's own boolean because Settings now has places worth linking to from
 * elsewhere — "Change what's offered" under a recipe question, "Edit recipes…" in a worktree's menu —
 * and each of those would otherwise need a callback threaded down to it from App.
 *
 * `show` is never offered to plugin hooks. Settings is where a misbehaving package is turned off, so
 * ⌘, the title-bar gear and every link here must reach it whatever is installed. A package asking
 * for Settings goes through `ui.settings.show`, whose hooks run in the registry before this is called.
 */
import { inPaneWindow, toMain } from '../window-role';

export type SettingsSection =
  | 'plugins'
  | 'appearance'
  | 'general'
  | 'notifications'
  | 'advanced'
  | 'recipes'
  | 'actions';

class SettingsView {
  open = $state(false);
  section = $state<SettingsSection>('appearance');
  /**
   * What to show when the section opens, if a link named something: a recipe or an action, or in
   * Plugins a tab (`installed`, `defaults`, `develop`) or a plugin's id. Plugins consumes it.
   */
  focus = $state<string | null>(null);

  show(section?: SettingsSection, focus: string | null = null): void {
    // A pane window mounts no Settings, so opening one there would set state nothing draws.
    if (inPaneWindow) {
      toMain({ kind: 'showSettings', section: section ?? null, focus });
      return;
    }
    if (section) this.section = section;
    this.focus = focus;
    this.open = true;
  }

  close(): void {
    this.open = false;
    this.focus = null;
  }
}

export const settingsView = new SettingsView();

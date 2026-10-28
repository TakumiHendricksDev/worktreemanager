/**
 * Whether Settings is open, and at which section.
 *
 * A store rather than App's own boolean because Settings now has places worth linking to from
 * elsewhere — "Change what's offered" under a recipe question, "Edit recipes…" in a worktree's menu —
 * and each of those would otherwise need a callback threaded down to it from App.
 */

export type SettingsSection =
  'appearance' | 'general' | 'notifications' | 'advanced' | 'recipes' | 'actions';

class SettingsView {
  open = $state(false);
  section = $state<SettingsSection>('appearance');
  /** The recipe or action to select when the section opens, if a link named one. */
  focus = $state<string | null>(null);

  show(section?: SettingsSection, focus: string | null = null): void {
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

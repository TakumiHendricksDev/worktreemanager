/**
 * The Code tab's chords, recognised in one place and named for tooltips in the same one.
 *
 * `App` listens and `CodeSurface` opens what they ask for. The names follow PyCharm's macOS keymap,
 * since that is the muscle memory this tab is for: ⇧⌘O a file, ⌘O a class, ⌥⌘O any symbol, and
 * ⇧⌘F Find in Files.
 */

export type CodeChord = 'file' | 'find';

export const GO_TO_FILE_SHORTCUT = '⇧⌘O';
export const FIND_IN_FILES_SHORTCUT = '⇧⌘F';

/**
 * Which chord a keydown is, or null.
 *
 * ⌘O is matched on `code`, the physical key, rather than `key`: with ⌥ held macOS turns `o` into
 * `ø`, so `key` would never say `o` for ⌥⌘O. Control is excluded, since ⌃⌘ chords belong to the
 * system and the sidebar toggle.
 */
export function chordOf(event: KeyboardEvent): CodeChord | null {
  if (!event.metaKey || event.ctrlKey) return null;
  if (event.code === 'KeyO' && event.shiftKey && !event.altKey) return 'file';
  if (event.code === 'KeyF' && event.shiftKey && !event.altKey) return 'find';
  return null;
}

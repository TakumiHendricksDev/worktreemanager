/**
 * Appearance: which palette, and light or dark.
 *
 * Two independent axes. `data-palette` picks the hue, `data-theme` picks the mode, and
 * every one of the six palettes works in both — so "follow the system" keeps meaning what
 * it meant before palettes existed.
 *
 * Four rules make this work without a flash of the wrong colours:
 *
 * 1. `index.html` resolves both axes from `localStorage` *before first paint*. Waiting for
 *    Rust to answer would show a light window on a dark desktop on every launch.
 * 2. `localStorage` is a cache, not the source of truth — `~/.config/wtm/config.toml` is,
 *    because the user edits it by hand. Both values are reconciled at startup.
 * 3. A *custom* palette's colours are cached too, not just its name. They come from Rust,
 *    so without the cache there is nothing to paint with on the first frame and every
 *    launch would flash the default palette. See `applyCustom`.
 * 4. The native window theme is set alongside the CSS, so the titlebar and the sidebar
 *    vibrancy match the content. Without that, an "Overlay" titlebar stays light while
 *    everything under it goes dark.
 */

import { getCurrentWindow } from '@tauri-apps/api/window';

import { commands } from '../ipc/commands';
import { errorMessage, type Palette } from '../ipc/types';

export type ThemeChoice = 'system' | 'light' | 'dark';
export type Resolved = 'light' | 'dark';

/**
 * The palettes compiled into the stylesheet, in picker order.
 *
 * A union type rather than `string[]`, because with a global stylesheet and no scoped
 * `<style>` blocks a wrong id is invisible to every tool in the repository — the same
 * reason `IconName` and the button variants are unions. This list and the `$palettes` map
 * in `settings/_palettes.scss` must agree; nothing checks that they do, so an id added in
 * one place and not the other renders as the default with no error.
 */
export type BuiltInPalette = 'pine' | 'clay' | 'slate' | 'harbor' | 'plum' | 'rose';

export const BUILT_IN_PALETTES: { id: BuiltInPalette; name: string }[] = [
  { id: 'pine', name: 'Pine' },
  { id: 'clay', name: 'Clay' },
  { id: 'slate', name: 'Slate' },
  { id: 'harbor', name: 'Harbor' },
  { id: 'plum', name: 'Plum' },
  { id: 'rose', name: 'Rose' },
];

export const DEFAULT_PALETTE: BuiltInPalette = 'pine';

const THEME_KEY = 'wtm.theme';
const PALETTE_KEY = 'wtm.palette';
/** The selected custom palette's colours, cached for the pre-paint script. See rule 3. */
const CUSTOM_KEY = 'wtm.palette.custom';

const THEME_PREF = 'ui.theme';
const PALETTE_PREF = 'ui.palette';

/**
 * The six properties a palette sets.
 *
 * Kept here as one list because a custom palette writes them inline and a built-in gets
 * them from a stylesheet rule, and the two must set exactly the same set — a property
 * written by one route and not cleared by the other is how a half-applied palette happens.
 */
const CUSTOM_PROPS = [
  '--palette-hue',
  '--palette-chroma',
  '--brand-300',
  '--brand-400',
  '--brand-500',
  '--brand-600',
] as const;

function isBuiltIn(id: string): id is BuiltInPalette {
  return BUILT_IN_PALETTES.some((p) => p.id === id);
}

function systemPrefers(): Resolved {
  return matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light';
}

function resolve(choice: ThemeChoice): Resolved {
  return choice === 'system' ? systemPrefers() : choice;
}

class ThemeStore {
  choice = $state<ThemeChoice>('system');
  resolved = $state<Resolved>('light');

  /** The selected palette id. May name a custom palette, or one this build removed. */
  palette = $state<string>(DEFAULT_PALETTE);
  /** Palettes from `[ui.palettes]`, including unusable ones — those carry `error`. */
  customPalettes = $state<Palette[]>([]);
  /** Surfaced by the shell's banner. A palette that fails to save must say so. */
  error = $state<string | null>(null);

  constructor() {
    // Trust what index.html already decided, so construction never causes a repaint.
    const root = document.documentElement;
    this.choice = (root.dataset.themeChoice as ThemeChoice | undefined) ?? 'system';
    this.resolved = resolve(this.choice);
    this.palette = root.dataset.palette ?? DEFAULT_PALETTE;
  }

  /** Every palette the picker offers, built-ins first. */
  all = $derived([
    ...BUILT_IN_PALETTES.map((p) => ({
      ...p,
      custom: false,
      error: null as string | null,
    })),
    ...this.customPalettes.map((p) => ({
      id: p.id,
      name: p.name,
      custom: true,
      error: p.error,
    })),
  ]);

  /** Reconcile with the persisted preferences and start following the OS. */
  async init(): Promise<void> {
    try {
      const [storedTheme, storedPalette] = await Promise.all([
        commands.getPref(THEME_PREF),
        commands.getPref(PALETTE_PREF),
      ]);
      if (storedTheme === 'system' || storedTheme === 'light' || storedTheme === 'dark') {
        this.choice = storedTheme;
      }
      if (storedPalette) this.palette = storedPalette;
    } catch {
      // Appearance is never worth blocking startup over.
    }

    // After the prefs, because a custom palette can only be resolved once we know which one
    // is selected — but before `apply`, so the first paint after boot is the right colour.
    try {
      this.customPalettes = await commands.listPalettes();
    } catch {
      // A missing list means the built-ins still work. Falling back is silent here because
      // `init` cannot put a banner on screen; a selected-but-missing palette is caught by
      // the resolution in `apply`.
    }

    this.apply();

    // Only meaningful while the choice is `system`, but the listener is cheap and
    // unconditional is one less state transition to get wrong.
    matchMedia('(prefers-color-scheme: dark)').addEventListener('change', () => {
      if (this.choice === 'system') this.apply();
    });
  }

  async set(choice: ThemeChoice): Promise<void> {
    this.choice = choice;
    this.apply();
    try {
      await commands.setPref(THEME_PREF, choice);
    } catch {
      // The in-memory and localStorage values already changed; failing to persist is a
      // nuisance, not a reason to revert what the user just chose.
    }
  }

  /**
   * Choose a palette.
   *
   * Reverts on a failed write, unlike `set` above. The difference is what a lost write
   * costs: a theme that fails to persist is re-derived from the OS next launch and nobody
   * notices, whereas a palette silently reverts to Pine tomorrow morning with no
   * explanation. Same reasoning as `workspace.setPreferredOpener`.
   */
  async setPalette(id: string): Promise<void> {
    const previous = this.palette;
    this.palette = id;
    this.error = null;
    this.apply();
    try {
      await commands.setPref(PALETTE_PREF, id);
    } catch (e) {
      this.palette = previous;
      this.apply();
      this.error = errorMessage(e);
    }
  }

  /** Re-read `[ui.palettes]` from disk. For after the config was hand-edited. */
  async reloadPalettes(): Promise<void> {
    try {
      this.customPalettes = await commands.listPalettes();
      this.apply();
    } catch (e) {
      this.error = errorMessage(e);
    }
  }

  /** system → light → dark → system. What the titlebar button cycles through. */
  cycle(): Promise<void> {
    const order: ThemeChoice[] = ['system', 'light', 'dark'];
    const next = order[(order.indexOf(this.choice) + 1) % order.length] ?? 'system';
    return this.set(next);
  }

  private apply(): void {
    this.resolved = resolve(this.choice);

    const root = document.documentElement;
    root.dataset.theme = this.resolved;
    root.dataset.themeChoice = this.choice;

    /*
     * A selected palette that is neither built in nor declared falls back to the default.
     * That happens to anyone who deletes a custom palette from their config without
     * changing the selection, and to a config copied from a build that had a palette this
     * one does not. Rendering the default beats rendering nothing.
     */
    const custom = this.customPalettes.find((p) => p.id === this.palette && !p.error);
    const known = custom !== undefined || isBuiltIn(this.palette);
    const effective = known ? this.palette : DEFAULT_PALETTE;

    root.dataset.palette = effective;
    this.applyCustom(custom);

    this.cache(THEME_KEY, this.choice);
    this.cache(PALETTE_KEY, effective);

    // Keep the native chrome in step with the content.
    void getCurrentWindow()
      .setTheme(this.resolved)
      .catch(() => {
        /* Not fatal; the CSS has already switched. */
      });
  }

  /**
   * Paint a custom palette, or clear one.
   *
   * Inline properties on `<html>`, which beat every selector in `_palettes.scss` — so a
   * custom palette renders through the same oklch ramp as a built-in rather than through a
   * parallel mechanism. `data-palette` is still set to its id so the attribute always names
   * what is on screen, even though no stylesheet rule matches it.
   *
   * Clearing has to be unconditional. Switching from a custom palette to a built-in one
   * leaves the inline properties winning over the built-in's rule otherwise, and the result
   * is a palette that is half one and half the other.
   */
  private applyCustom(palette: Palette | undefined): void {
    const style = document.documentElement.style;

    if (!palette) {
      for (const prop of CUSTOM_PROPS) style.removeProperty(prop);
      this.cache(CUSTOM_KEY, null);
      return;
    }

    const values = [
      String(palette.hue),
      String(palette.chroma),
      ...palette.brand,
    ] satisfies string[];

    CUSTOM_PROPS.forEach((prop, i) => style.setProperty(prop, values[i] ?? ''));
    this.cache(CUSTOM_KEY, values.join(' '));
  }

  /**
   * Write one pre-paint cache entry.
   *
   * Every one of these is a cache of something Rust already knows, so a storage failure
   * costs the no-flash optimisation and nothing else. Private browsing and a full quota
   * both land here.
   */
  private cache(key: string, value: string | null): void {
    try {
      if (value === null) localStorage.removeItem(key);
      else localStorage.setItem(key, value);
    } catch {
      /* The app survives without it; the first frame after a relaunch may not match. */
    }
  }
}

export const theme = new ThemeStore();

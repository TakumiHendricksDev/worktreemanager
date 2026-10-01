/**
 * The icon set, as path data on a 16×16 grid.
 *
 * # Why these are SVG and not the text glyphs they replace
 *
 * Every icon in this app used to be a literal Unicode character in a `<span>` — `⌄` for the
 * dropdown caret, `⌕` for search, `✕` for close, `★`/`☆` for the favourite star, `＋` for the
 * new-worktree button. Centring one is not possible in the general case, and the dropdown
 * carets are where it showed:
 *
 *   1. `align-items: center` centres a glyph's **line box**, not its **ink**. `⌄` (U+2304 DOWN
 *      ARROWHEAD) draws entirely above the baseline, so the box centres and the arrow does not.
 *   2. `line-height: 1`, which both carets set, makes it worse rather than better — it produces
 *      negative half-leading and hands baseline placement to the font's own metrics.
 *   3. None of these characters is in `--font-ui`. They arrive from whatever fallback the
 *      platform picks, whose metrics are unrelated and **differ between macOS and Linux**. So
 *      the offset was never one number to correct.
 *
 * Two of them were worse than mis-centred. `＋` is U+FF0B FULLWIDTH PLUS SIGN, a CJK-width
 * character roughly twice the advance of `+`. `⌕` is U+2315 TELEPHONE RECORDER, which merely
 * resembles a magnifier and is absent from Cantarell, Ubuntu and DejaVu Sans — a tofu box on
 * the entire Linux font stack, which nobody would ever have seen from a Mac.
 *
 * An SVG's box **is** its ink box, so `place-items: center` genuinely centres it, identically
 * on every platform and in every font. That is the whole reason for this file.
 *
 * # Why some glyphs deliberately stay as text
 *
 * Six do, and the reasons are worth stating so nobody "finishes the job":
 *
 *   - `⚠` in a row of the project picker, a native menu. A menu row is text and nothing
 *     else — an SVG cannot go there at all.
 *   - `⚠` appended to a button label by a ternary, where it flows inline with the text.
 *   - `↑n↓n` and `● modified` in the sidebar's flag row, which sets `--font-mono`. Arrows and
 *     bullets in a monospace face are metrically correct by construction, which is the one
 *     case where a text glyph is the *right* answer.
 *   - `••••••••` masking an environment value. That is data, not an icon.
 *
 * # Grammar
 *
 * 16×16 viewBox, 1.5 stroke, round caps and joins. Coordinates stay between 1.5 and 14.5 so
 * the stroke never touches the edge and icons of different shapes read as the same size.
 */

export type IconName =
  | 'chevron-down'
  | 'chevron-left'
  | 'chevron-right'
  | 'chevron-up'
  | 'check'
  | 'close'
  | 'collapse'
  | 'comment'
  | 'external'
  | 'globe'
  | 'file'
  | 'folder'
  | 'gauge'
  | 'grip'
  | 'locate'
  | 'star'
  | 'star-outline'
  | 'plus'
  | 'pop-in'
  | 'pop-out'
  | 'restart'
  | 'search'
  | 'settings'
  | 'sidebar'
  | 'split-right'
  | 'symlink'
  | 'terminal'
  | 'theme-system'
  | 'theme-light'
  | 'theme-dark'
  | 'bolt'
  | 'mic'
  | 'more'
  | 'more-vertical'
  | 'warn';

export interface IconSpec {
  /** Drawn with `stroke: currentColor; fill: none`. */
  stroke?: string;
  /** Drawn with `fill: currentColor` beneath the stroke. */
  fill?: string;
}

/**
 * A five-pointed star, outer radius 6.2 and inner 2.55 about the centre.
 *
 * Shared by both star icons on purpose. `★` and `☆` have different advance widths in most
 * fonts, so the favourite toggle used to shift horizontally when you clicked it. One path
 * used two ways cannot do that.
 */
const STAR =
  'M8.00 1.80 L9.50 5.94 L13.90 6.08 L10.43 8.79 L11.64 13.02 ' +
  'L8.00 10.55 L4.36 13.02 L5.57 8.79 L2.10 6.08 L6.50 5.94 Z';

export const icons: Record<IconName, IconSpec> = {
  'chevron-down': { stroke: 'M4 6.25 L8 10.25 L12 6.25' },
  'chevron-left': { stroke: 'M9.75 4 L5.75 8 L9.75 12' },
  'chevron-right': { stroke: 'M6.25 4 L10.25 8 L6.25 12' },
  'chevron-up': { stroke: 'M4 9.75 L8 5.75 L12 9.75' },
  check: { stroke: 'M3.5 8.25 L6.5 11.25 L12.5 4.75' },
  /**
   * A lightning bolt, filled rather than stroked.
   *
   * Filled because at 11px — the size the model picker's row uses — a 1.5 stroke on a shape this
   * narrow closes its own interior and reads as a blob. The grammar's coordinate bounds still hold;
   * it is the stroke rule that does not survive the scale, which is the same exception `star`
   * already makes.
   */
  bolt: { fill: 'M9.25 1.5 L4 9 H7.25 L6.75 14.5 L12 7 H8.75 Z' },
  /**
   * A microphone: a stroked capsule, its cradle, and the stand.
   *
   * Stroked rather than filled, unlike `bolt` — at 14px the capsule is wide enough for a 1.5
   * stroke to leave an interior, and a filled mic reads as a "recording" state before anything is
   * recording. The state is carried by the button's own fill instead.
   */
  mic: {
    stroke:
      'M8 2.25 A1.75 1.75 0 0 1 9.75 4 V7.5 A1.75 1.75 0 0 1 6.25 7.5 V4 ' +
      'A1.75 1.75 0 0 1 8 2.25 Z M3.75 7.25 A4.25 4.25 0 0 0 12.25 7.25 M8 11.5 V14',
  },
  /*
   * A dial: three quarters of a ring, open at the bottom, and a needle leaning right. The
   * usage-limits button in the title bar.
   *
   * Three quarters rather than a half, because a half ring fills six of the sixteen units and sat
   * visibly smaller than `settings` and the theme glyphs beside it.
   */
  gauge: { stroke: 'M3.76 12.74 A6 6 0 1 1 12.24 12.74 M8 8.5 L10.5 5.5' },
  /*
   * Three dots in a row: more actions than there is room for.
   *
   * Filled discs, not stroked dots. A dot drawn as a zero-length stroke is the failure `grip`
   * records — at 1.5 it relies on the round cap to render at all and blurs into its neighbour. Discs
   * of radius 1.25 on a 4.5 pitch leave a clear gap between each at 13px.
   */
  more: {
    fill:
      'M2.25 8 A1.25 1.25 0 1 0 4.75 8 A1.25 1.25 0 1 0 2.25 8 Z ' +
      'M6.75 8 A1.25 1.25 0 1 0 9.25 8 A1.25 1.25 0 1 0 6.75 8 Z ' +
      'M11.25 8 A1.25 1.25 0 1 0 13.75 8 A1.25 1.25 0 1 0 11.25 8 Z',
  },
  // The same three dots stood on end: PyCharm's ⋮ at the end of its editor tabs, which the Code
  // tab's strip copies so the control is where a PyCharm user's hand already goes.
  'more-vertical': {
    fill:
      'M8 2.25 A1.25 1.25 0 1 0 8 4.75 A1.25 1.25 0 1 0 8 2.25 Z ' +
      'M8 6.75 A1.25 1.25 0 1 0 8 9.25 A1.25 1.25 0 1 0 8 6.75 Z ' +
      'M8 11.25 A1.25 1.25 0 1 0 8 13.75 A1.25 1.25 0 1 0 8 11.25 Z',
  },
  close: { stroke: 'M4.25 4.25 L11.75 11.75 M11.75 4.25 L4.25 11.75' },
  // Two chevrons folding up: every open folder closes. The Code tab's tree header.
  collapse: { stroke: 'M4.5 7.25 L8 3.75 L11.5 7.25 M4.5 12.25 L8 8.75 L11.5 12.25' },
  /** A speech bubble with its tail at the lower left: a comment left *on* something. */
  comment: { stroke: 'M2.5 3.25 H13.5 V10.25 H7.5 L4.5 12.75 V10.25 H2.5 Z' },
  /** A box with an arrow leaving its top-right corner — open elsewhere. */
  external: {
    stroke: 'M7 3.5 H3.5 V12.5 H12.5 V9 M9.25 3.5 H12.5 V6.75 M12.5 3.5 L7.5 8.5',
  },
  /*
   * A globe: a circle, its equator and one meridian. Three strokes rather than the usual five
   * because at 1.5 stroke on a 16 grid a second meridian closes the gaps into a blob.
   */
  globe: {
    stroke:
      'M8 1.75 A6.25 6.25 0 1 0 8 14.25 A6.25 6.25 0 1 0 8 1.75 M1.75 8 H14.25 M8 1.75 C5.6 4.5 5.6 11.5 8 14.25',
  },
  /** A triangle, the same shape warnings already used as a `!` glyph in preflight lists. */
  warn: { stroke: 'M8 3 L13.25 13.25 H2.75 Z M8 6.75 V10 M8 11.5 V12.25' },
  file: { stroke: 'M4 1.75 H9.25 L12 4.5 V14.25 H4 Z M9.25 1.75 V4.5 H12' },
  folder: { stroke: 'M2 3.75 H6.25 L7.75 5.25 H14 V12.75 H2 Z' },
  // A crosshair, PyCharm's ⌖: find the open file in the tree.
  locate: {
    stroke:
      'M8 4.75 A3.25 3.25 0 1 0 8 11.25 A3.25 3.25 0 1 0 8 4.75 ' +
      'M8 1.75 V4.75 M8 11.25 V14.25 M1.75 8 H4.75 M11.25 8 H14.25',
  },
  /*
   * Three bars, not six dots.
   *
   * The six-dot grip is the more expected glyph and it fails the grammar above: two columns of three
   * dots on a 16 grid are zero-length subpaths relying on `stroke-linecap: round` to render at all, and
   * at 1.5 stroke they merge into two blurred vertical smudges — the same failure the cog had. Three
   * short rules survive the size, and they are what Lucide ships for a drag handle.
   */
  grip: { stroke: 'M5.5 5 H10.5 M5.5 8 H10.5 M5.5 11 H10.5' },
  star: { fill: STAR, stroke: STAR },
  'star-outline': { stroke: STAR },
  plus: { stroke: 'M8 3.5 L8 12.5 M3.5 8 L12.5 8' },
  /**
   * A box with an arrow arriving at it from the upper left: put the pane back in the main window.
   *
   * The mirror of `external`, deliberately — that one leaves through the upper-right corner, and
   * the two appear on the same kind of pane. An arrow going *in* is the one reading neither of the
   * others has.
   */
  'pop-in': { stroke: 'M9 3.5 H12.5 V12.5 H3.5 V9 M2.5 2.5 L8 8 M8 4.75 V8 H4.75' },
  /**
   * Two windows, the second lifted over the first's corner: move the pane into its own window.
   *
   * Not `external`'s arrow, which already means "open this in another app" on a browser pane's
   * toolbar — two controls a few pixels apart must not look alike. The back window is drawn only
   * where it shows, so the front one reads as on top without a fill.
   */
  'pop-out': { stroke: 'M6.5 6.5 H2.5 V13.5 H9.5 V9.5 M6.5 2.5 H13.5 V9.5 H6.5 Z' },
  /*
   * An arc with one arrowhead, not a closed loop with two.
   *
   * The gap is what makes it read as *restart* rather than as "syncing": a full circle of arrows is
   * the refresh idiom, and this control ends a process and starts another one. Three-quarters of a
   * circle leaves room for the head at 1.5 stroke on a 16 grid, which two heads did not — they
   * collided with the arc at this size, which is the same test the cog and the terminal box failed.
   */
  restart: {
    stroke: 'M12.9 5.4 A5.6 5.6 0 1 0 13.1 10.2 M12.9 5.4 L12.9 2.4 M12.9 5.4 L9.9 5.4',
  },
  search: {
    stroke:
      'M7.25 2.5 A4.75 4.75 0 1 0 7.25 12 A4.75 4.75 0 1 0 7.25 2.5 M10.9 10.9 L13.5 13.5',
  },
  /*
   * A window with a narrow column ruled off its left edge: the sidebar.
   *
   * The same glyph whether the sidebar is showing or not, which is how the macOS toolbar button
   * behaves. A chevron that flipped with the state was what it replaced, and a control whose icon
   * changes when you press it reads as two controls. Unfilled, unlike `split-right`: that one's fill
   * says where a new pane lands, and there is no direction to say here.
   */
  sidebar: { stroke: 'M2.5 3 H13.5 V13 H2.5 Z M6.25 3 V13' },
  /*
   * Two panes side by side, the right one filled.
   *
   * A large simple shape, which is what survives 1.5 stroke on a 16 grid — the same test the cog and
   * the terminal box failed. The fill is what makes it read as a *direction* rather than as a generic
   * two-column glyph: without it the icon means "columns", with it it means "the new one goes there".
   */
  'split-right': {
    stroke: 'M2.5 3 H13.5 V13 H2.5 Z M8 3 V13',
    fill: 'M8 3 H13.5 V13 H8 Z',
  },
  // A hooked arrow, ↪: this entry is a link to somewhere else.
  symlink: {
    stroke: 'M4 3.5 V8.25 A2.75 2.75 0 0 0 6.75 11 H12.25 M9.75 8.5 L12.25 11 L9.75 13.5',
  },
  /*
   * Sliders, not a cog, and the grammar above is the reason.
   *
   * A cog is the more expected glyph, and it was drawn first: a circle with eight teeth on a
   * 16 grid leaves each tooth about 1.6 long against a 1.5 stroke, so they render as a ring
   * of blurred dots. Dropping to six teeth trades one kind of illegible for another.
   *
   * Two rails and two knobs survive the size, and "adjust these" is the same idea a cog is
   * standing in for. It is what Lucide and Feather both ship for preferences panels for the
   * same reason.
   */
  settings: {
    stroke:
      'M2.5 5.5 H13.5 M2.5 10.5 H13.5 ' +
      'M10 3.9 A1.6 1.6 0 1 0 10 7.1 A1.6 1.6 0 1 0 10 3.9 ' +
      'M6 8.9 A1.6 1.6 0 1 0 6 12.1 A1.6 1.6 0 1 0 6 8.9',
  },
  /*
   * A prompt and a command line. No window frame, and that is the grammar again.
   *
   * The expected glyph is a rounded box with a `>_` inside it, and it was drawn first: a box
   * inset to 2.5–13.5 leaves the chevron about three units of arm against a 1.5 stroke, which
   * renders as a smudge with a border round it — the same failure the cog had. Two marks
   * survive the size, and a chevron over a rule is what Lucide and Feather both ship for a
   * console.
   *
   * The rule sits on the chevron's lower arm rather than on the baseline, so the pair reads as
   * one prompt rather than as two unrelated strokes.
   */
  terminal: { stroke: 'M3.5 3.75 L7.75 8 L3.5 12.25 M9.25 12.25 L13 12.25' },
  // A circle with its leading half filled — the same "follows the system" idiom as `◐`, which
  // is the character it replaces.
  'theme-system': {
    fill: 'M8 2 A6 6 0 0 0 8 14 Z',
    stroke: 'M8 2 A6 6 0 1 0 8 14 A6 6 0 1 0 8 2',
  },
  'theme-light': {
    stroke:
      'M8 5.25 A2.75 2.75 0 1 0 8 10.75 A2.75 2.75 0 1 0 8 5.25 ' +
      'M8 1.5 L8 2.75 M8 13.25 L8 14.5 M1.5 8 L2.75 8 M13.25 8 L14.5 8 ' +
      'M3.4 3.4 L4.28 4.28 M11.72 11.72 L12.6 12.6 ' +
      'M12.6 3.4 L11.72 4.28 M4.28 11.72 L3.4 12.6',
  },
  'theme-dark': { stroke: 'M13 10.1 A5.6 5.6 0 0 1 5.9 3 A5.75 5.75 0 1 0 13 10.1 Z' },
};

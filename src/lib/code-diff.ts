/**
 * The viewer's change markers: which lines of a file differ from the Changes view's revision.
 *
 * From `git diff -U0` hunks, drawn the way an IDE draws them — a bar beside added and changed
 * lines, a wedge where lines were deleted — in a gutter of their own. Clicking a marker shows what
 * the lines were, which is the question a reviewer is asking when they look at one.
 *
 * The pure part (`markersOf`) is kept apart from the CodeMirror part, since there is no JS test
 * runner and the arithmetic is the part most likely to be wrong: a hunk with no new lines is a
 * deletion *after* its line, and one with no old lines is an insertion.
 */

import { RangeSet, StateEffect, StateField, type Extension } from '@codemirror/state';
import {
  EditorView,
  GutterMarker,
  gutter,
  showTooltip,
  type Tooltip,
} from '@codemirror/view';

import type { CodeHunk } from './ipc/types';

export type MarkKind = 'added' | 'modified' | 'deleted';

/** One marker: a kind on a 1-based line, and the hunk it came from. */
export interface Mark {
  line: number;
  kind: MarkKind;
  hunk: CodeHunk;
}

/**
 * Every marked line in a file of `lines` lines.
 *
 * A pure insertion marks each new line added; a replacement marks each new line modified; a
 * deletion marks the line after which the lines went — or the first line, for a deletion at the
 * very top, which `-U0` reports as after line 0.
 */
export function markersOf(hunks: readonly CodeHunk[], lines: number): Mark[] {
  const marks: Mark[] = [];
  for (const hunk of hunks) {
    if (hunk.newLines === 0) {
      marks.push({
        line: Math.min(Math.max(hunk.newStart, 1), lines),
        kind: 'deleted',
        hunk,
      });
      continue;
    }
    const kind: MarkKind = hunk.oldLines === 0 ? 'added' : 'modified';
    const last = Math.min(hunk.newStart + hunk.newLines - 1, lines);
    for (let line = hunk.newStart; line <= last; line += 1)
      marks.push({ line, kind, hunk });
  }
  return marks;
}

/** The whole file as one added hunk: what an untracked file is against any revision. */
export function allAdded(lines: number): CodeHunk[] {
  return lines === 0
    ? []
    : [{ oldStart: 0, oldLines: 0, newStart: 1, newLines: lines, removed: [] }];
}

/** The lines that start each hunk, for stepping between changes. */
export function hunkStarts(hunks: readonly CodeHunk[]): number[] {
  return hunks.map((hunk) => Math.max(hunk.newStart, 1));
}

class ChangeMarker extends GutterMarker {
  /** On the line's gutter element, so a run of marked lines draws one continuous bar. */
  override elementClass: string;

  constructor(
    readonly kind: MarkKind,
    readonly hunk: CodeHunk,
  ) {
    super();
    this.elementClass = `c-code-editor__change c-code-editor__change--${kind}`;
  }

  override eq(other: GutterMarker): boolean {
    return (
      other instanceof ChangeMarker && other.kind === this.kind && other.hunk === this.hunk
    );
  }
}

/** Set a file's hunks. `null` clears them: the file is unchanged, or the view is not comparing. */
export const setHunks = StateEffect.define<readonly CodeHunk[] | null>();
/** Open the tooltip for the hunk at a position, or close it. */
const openHunk = StateEffect.define<{ pos: number; hunk: CodeHunk } | null>();

const hunksField = StateField.define<readonly CodeHunk[]>({
  create: () => [],
  update(value, tr) {
    for (const effect of tr.effects) if (effect.is(setHunks)) return effect.value ?? [];
    return value;
  },
});

const markersField = StateField.define<RangeSet<GutterMarker>>({
  create: () => RangeSet.empty,
  update(value, tr) {
    if (!tr.docChanged && !tr.effects.some((e) => e.is(setHunks))) return value;
    const doc = tr.state.doc;
    const marks = markersOf(tr.state.field(hunksField), doc.lines);
    return RangeSet.of(
      marks.map((mark) =>
        new ChangeMarker(mark.kind, mark.hunk).range(doc.line(mark.line).from),
      ),
      true,
    );
  },
});

const tooltipField = StateField.define<Tooltip | null>({
  create: () => null,
  update(value, tr) {
    for (const effect of tr.effects) {
      if (effect.is(openHunk))
        return effect.value ? oldLinesTooltip(effect.value.pos, effect.value.hunk) : null;
      if (effect.is(setHunks)) return null;
    }
    // Moving the caret, clicking into the text or stepping to another change is moving on.
    return tr.docChanged || tr.selection ? null : value;
  },
  provide: (field) => showTooltip.from(field),
});

function oldLinesTooltip(pos: number, hunk: CodeHunk): Tooltip {
  return {
    pos,
    above: false,
    create: () => {
      const dom = document.createElement('div');
      dom.className = 'c-code-editor__old';
      const head = document.createElement('div');
      head.className = 'c-code-editor__old-head';
      head.textContent =
        hunk.oldLines === 0
          ? 'Added lines — there was nothing here before.'
          : `Was ${hunk.oldLines} line${hunk.oldLines === 1 ? '' : 's'}:`;
      dom.append(head);
      if (hunk.removed.length > 0) {
        const pre = document.createElement('pre');
        pre.className = 'c-code-editor__old-lines';
        pre.textContent = hunk.removed.join('\n');
        dom.append(pre);
      }
      return { dom };
    },
  };
}

/** The change gutter and its tooltip, for the Code tab's viewer. */
export function changeGutter(): Extension {
  return [
    hunksField,
    markersField,
    tooltipField,
    gutter({
      class: 'c-code-editor__changes',
      markers: (view) => view.state.field(markersField),
      domEventHandlers: {
        mousedown(view, block) {
          const markers = view.state.field(markersField);
          let found: ChangeMarker | null = null;
          markers.between(block.from, block.from, (_from, _to, marker) => {
            if (marker instanceof ChangeMarker) found = marker;
          });
          const open = view.state.field(tooltipField);
          const hit = found as ChangeMarker | null;
          view.dispatch({
            effects: openHunk.of(
              hit && !(open && open.pos === block.from)
                ? { pos: block.from, hunk: hit.hunk }
                : null,
            ),
          });
          return true;
        },
      },
    }),
    EditorView.domEventHandlers({
      keydown(event, view) {
        if (event.key === 'Escape' && view.state.field(tooltipField)) {
          view.dispatch({ effects: openHunk.of(null) });
          return true;
        }
        return false;
      },
    }),
  ];
}

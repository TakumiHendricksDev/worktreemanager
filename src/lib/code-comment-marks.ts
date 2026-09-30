/**
 * Comments in the Code tab's viewer: the cards under the lines they are about, the tint on those
 * lines, the offer beside a selection, and ⌥⌘C.
 *
 * A module of its own rather than part of `CodeViewer`, because the widget is a class and a
 * Svelte script cannot hold TypeScript's parameter properties — and because it is a CodeMirror
 * extension first, which the viewer only has to install and feed.
 *
 * Block widgets have to come from a state field (a view plugin may not change the document's
 * height), and the field rebuilds them from its input whenever that or the text changes: a comment
 * is about line numbers, not positions to map through an edit.
 */

import {
  StateEffect,
  StateField,
  type EditorState,
  type Extension,
  type Range,
} from '@codemirror/state';
import {
  Decoration,
  EditorView,
  WidgetType,
  keymap,
  showTooltip,
  type DecorationSet,
  type Tooltip,
} from '@codemirror/view';
import { mount, unmount } from 'svelte';

import CodeCommentCard from './components/CodeCommentCard.svelte';
import type { CodeComment } from './ipc/types';

/** What the comments in the viewer can ask for. `CodeSurface` does it; the viewer only draws. */
export interface CommentActions {
  compose: (start: number, end: number) => void;
  save: (text: string, excerpt: string) => Promise<void>;
  cancel: () => void;
  update: (id: number, text: string) => Promise<void>;
  resolve: (id: number, resolved: boolean) => void;
  remove: (id: number) => void;
  ask: (start: number, end: number, excerpt: string) => void;
}

export interface CommentInput {
  comments: readonly CodeComment[];
  composing: { start: number; end: number } | null;
}

export const setComments = StateEffect.define<CommentInput>();

const commentInput = StateField.define<CommentInput>({
  create: () => ({ comments: [], composing: null }),
  update(value, tr) {
    for (const effect of tr.effects) if (effect.is(setComments)) return effect.value;
    return value;
  },
});

/** The text of lines `start..=end` in a state, as an excerpt compares it; null past the end. */
export function linesText(state: EditorState, start: number, end: number): string | null {
  if (start < 1 || end > state.doc.lines || end < start) return null;
  return state.doc.sliceString(state.doc.line(start).from, state.doc.line(end).to);
}

export function rangeLabel(start: number, end: number): string {
  return start === end ? String(start) : `${start}–${end}`;
}

/** The lines a selection covers, not counting a line it only touches at its very start. */
export function selectedLines(state: EditorState): { start: number; end: number } {
  const range = state.selection.main;
  const first = state.doc.lineAt(range.from);
  let last = state.doc.lineAt(range.to);
  if (!range.empty && range.to === last.from && last.number > first.number) {
    last = state.doc.line(last.number - 1);
  }
  return { start: first.number, end: last.number };
}

class CommentWidget extends WidgetType {
  private app: ReturnType<typeof mount> | null = null;
  readonly comment: CodeComment | null;
  readonly start: number;
  readonly end: number;
  readonly outdated: boolean;
  private readonly actions: () => CommentActions;

  constructor(
    comment: CodeComment | null,
    start: number,
    end: number,
    outdated: boolean,
    actions: () => CommentActions,
  ) {
    super();
    this.comment = comment;
    this.start = start;
    this.end = end;
    this.outdated = outdated;
    this.actions = actions;
  }

  override eq(other: CommentWidget): boolean {
    const a = this.comment;
    const b = other.comment;
    const same =
      a === b ||
      (a !== null &&
        b !== null &&
        a.id === b.id &&
        a.text === b.text &&
        a.status === b.status &&
        a.sentTo === b.sentTo &&
        a.note === b.note);
    return (
      same &&
      this.start === other.start &&
      this.end === other.end &&
      this.outdated === other.outdated
    );
  }

  toDOM(view: EditorView): HTMLElement {
    const dom = document.createElement('div');
    dom.className = 'c-code-editor__comment';
    const comment = this.comment;
    const actions = this.actions;
    this.app = mount(CodeCommentCard, {
      target: dom,
      props: {
        comment,
        lines: rangeLabel(this.start, this.end),
        outdated: this.outdated,
        onsave: async (text: string) => {
          if (comment) {
            await actions().update(comment.id, text);
            return;
          }
          await actions().save(text, linesText(view.state, this.start, this.end) ?? '');
          // The selection was for writing this comment; left in place, its offer would sit over
          // the card that was just made.
          view.dispatch({ selection: { anchor: view.state.selection.main.head } });
        },
        oncancel: () => actions().cancel(),
        onresolve: (resolved: boolean) => {
          if (comment) actions().resolve(comment.id, resolved);
        },
        ondelete: () => {
          if (comment) actions().remove(comment.id);
        },
      },
    });
    return dom;
  }

  override destroy(): void {
    if (this.app) void unmount(this.app);
    this.app = null;
  }

  /** Typing and clicking inside the card are the card's, not the editor's. */
  override ignoreEvent(): boolean {
    return true;
  }
}

/**
 * Everything above as one extension. `actions` and `askLabel` are read when used rather than
 * captured, so the viewer's latest props are the ones that act.
 */
export function commentExtension(
  actions: () => CommentActions,
  askLabel: () => string | null,
): Extension {
  const marks = StateField.define<DecorationSet>({
    create: () => Decoration.none,
    update(value, tr) {
      if (!tr.docChanged && !tr.effects.some((e) => e.is(setComments))) return value;
      const state = tr.state;
      const { comments, composing } = state.field(commentInput);
      const ranges: Range<Decoration>[] = [];
      const add = (comment: CodeComment | null, start: number, end: number) => {
        const text = linesText(state, start, end);
        if (text === null) return;
        const outdated = comment !== null && text !== comment.excerpt;
        const tint = comment?.status === 'resolved' ? 'is-resolved' : 'is-open';
        // A tint on at most five hundred lines: a comment on more is a comment on the file.
        for (let line = start; line <= Math.min(end, start + 500); line += 1) {
          ranges.push(
            Decoration.line({ class: `c-code-editor__commented ${tint}` }).range(
              state.doc.line(line).from,
            ),
          );
        }
        ranges.push(
          Decoration.widget({
            widget: new CommentWidget(comment, start, end, outdated, actions),
            block: true,
            side: 1,
          }).range(state.doc.line(end).to),
        );
      };
      for (const comment of comments) add(comment, comment.start, comment.end);
      if (composing) add(null, composing.start, composing.end);
      return Decoration.set(ranges, true);
    },
    provide: (field) => EditorView.decorations.from(field),
  });

  /*
   * The two things to do with a selection, offered beside it: comment on it, or ask the agent about
   * it now. The buttons act on mousedown with the default prevented, so the click does not
   * collapse the selection it is about.
   */
  const tips = (state: EditorState): readonly Tooltip[] => {
    const range = state.selection.main;
    if (range.empty || state.field(commentInput).composing) return [];
    return [
      {
        pos: range.head,
        above: range.head < range.anchor,
        strictSide: true,
        arrow: false,
        create: (view) => {
          const dom = document.createElement('div');
          dom.className = 'c-code-editor__actions';
          const button = (label: string, title: string, run: () => void) => {
            const b = document.createElement('button');
            b.type = 'button';
            b.className = 'c-code-editor__action';
            b.textContent = label;
            b.title = title;
            b.addEventListener('mousedown', (event) => {
              event.preventDefault();
              run();
            });
            dom.append(b);
          };
          const { start, end } = selectedLines(view.state);
          button('Comment', 'Comment on these lines (⌥⌘C)', () =>
            actions().compose(start, end),
          );
          const agent = askLabel();
          if (agent) {
            button(`Ask ${agent}`, `Put these lines in ${agent}'s message box`, () =>
              actions().ask(start, end, linesText(view.state, start, end) ?? ''),
            );
          }
          return { dom };
        },
      },
    ];
  };
  const selectionTip = StateField.define<readonly Tooltip[]>({
    create: tips,
    update(value, tr) {
      const touched =
        tr.docChanged || tr.selection || tr.effects.some((e) => e.is(setComments));
      return touched ? tips(tr.state) : value;
    },
    provide: (field) => showTooltip.computeN([field], (state) => state.field(field)),
  });

  return [
    commentInput,
    marks,
    selectionTip,
    keymap.of([
      {
        key: 'Mod-Alt-c',
        run: (view) => {
          const { start, end } = selectedLines(view.state);
          actions().compose(start, end);
          return true;
        },
      },
    ]),
  ];
}

<script lang="ts">
  /**
   * The Code tab's file view: CodeMirror, read-only.
   *
   * # Read-only, but focusable
   *
   * `EditorState.readOnly`, not `EditorView.editable.of(false)`. The second makes the content
   * unfocusable, which takes the caret, keyboard selection, copy and the search panel with it —
   * everything a reader does. Read-only refuses typing and keeps the rest, and the search panel
   * drops its replace controls on its own.
   *
   * # One view, a state per tab
   *
   * There is a single `EditorView`, and each open file keeps its own `EditorState` in `states`,
   * swapped in with `setState` when its tab is shown. A state is where CodeMirror keeps the
   * selection, the folds and the parse, so switching back to a tab finds it as it was left without a
   * second editor per tab; the scroll position is the one thing a state does not hold, so it is kept
   * beside it.
   */
  import { Compartment, EditorState, StateEffect, StateField } from '@codemirror/state';
  import type { Extension } from '@codemirror/state';
  import {
    Decoration,
    EditorView,
    highlightActiveLine,
    highlightActiveLineGutter,
    keymap,
    type DecorationSet,
  } from '@codemirror/view';
  import { foldGutter, foldKeymap } from '@codemirror/language';
  import { openSearchPanel, search, searchKeymap } from '@codemirror/search';
  import { defaultKeymap } from '@codemirror/commands';
  import { onMount, untrack } from 'svelte';

  import { changeGutter, hunkStarts, setHunks } from '../code-diff';
  import { readingExtensions } from '../code-editor';
  import type { CodeComment, CodeHunk } from '../ipc/types';
  import {
    commentExtension,
    linesText,
    rangeLabel,
    selectedLines,
    setComments,
    type CommentActions,
    type CommentInput,
  } from '../code-comment-marks';
  import { item, popUp, separator } from '../native-menu';
  import { reference } from '../code-review';
  import { languageFor } from '../code-languages';
  import { code, type FileState } from '../state/code.svelte';

  const {
    worktreeId,
    path,
    file,
    hunks,
    comments,
    composing,
    askLabel,
    actions,
    ondefinition,
  }: {
    worktreeId: string;
    /** The file on screen, or null when no tab is open. */
    path: string | null;
    file: FileState | null;
    /** How the file differs from what Changes compares with, for the gutter. Null: no markers. */
    hunks: readonly CodeHunk[] | null;
    /** This file's comments. */
    comments: readonly CodeComment[];
    /** Lines a new comment is being written on, in this file. */
    composing: { start: number; end: number } | null;
    /** The agent Ask would draft into, or null when the worktree has none. */
    askLabel: string | null;
    actions: CommentActions;
    /** ⌘-click or ⌘B on a name: go to where it is defined. */
    ondefinition: (name: string) => void;
  } = $props();

  /** The identifier at a position, or null over whitespace and punctuation. */
  function nameAt(state: EditorState, pos: number): string | null {
    const word = state.wordAt(pos);
    const text = word ? state.sliceDoc(word.from, word.to) : '';
    return /^[A-Za-z_$][\w$]*$/.test(text) ? text : null;
  }

  const definitions: Extension = [
    EditorView.domEventHandlers({
      mousedown(event, editor) {
        if (!event.metaKey || event.button !== 0) return false;
        const pos = editor.posAtCoords({ x: event.clientX, y: event.clientY });
        const name = pos === null ? null : nameAt(editor.state, pos);
        if (!name) return false;
        event.preventDefault();
        ondefinition(name);
        return true;
      },
    }),
    keymap.of([
      {
        key: 'Mod-b',
        run: (editor) => {
          const name = nameAt(editor.state, editor.state.selection.main.head);
          if (name) ondefinition(name);
          return name !== null;
        },
      },
    ]),
  ];

  interface Kept {
    state: EditorState;
    text: string;
    scroll: number;
  }

  /** Every open file's editor state, by worktree and path. Not `$state`: nothing renders it. */
  const states = new Map<string, Kept>();
  const language = new Compartment();
  let host = $state<HTMLDivElement | null>(null);
  let view: EditorView | null = null;
  let shown: string | null = null;
  let hasSelection = $state(false);
  let lastReveal = 0;

  const keyOf = (p: string) => `${worktreeId}\0${p}`;
  const text = $derived(
    file?.status === 'ready' ? file.file.text : file?.status === 'base' ? file.text : null,
  );
  /** Which hunks each state was last given, so an unchanged answer is not dispatched again. */
  const given = new Map<string, readonly CodeHunk[] | null>();

  /*
   * A line that briefly lights up where a search result or a link landed, so the eye finds it.
   * A decoration in a field, because the viewer is read-only and a decoration is the only kind of
   * mark CodeMirror draws that is not the document.
   */
  const flash = StateEffect.define<number | null>();
  const flashField = StateField.define<DecorationSet>({
    create: () => Decoration.none,
    update(marks, tr) {
      for (const effect of tr.effects) {
        if (!effect.is(flash)) continue;
        if (effect.value === null) return Decoration.none;
        const line = tr.state.doc.lineAt(effect.value);
        return Decoration.set([
          Decoration.line({ class: 'c-code-editor__flash' }).range(line.from),
        ]);
      }
      return marks.map(tr.changes);
    },
    provide: (field) => EditorView.decorations.from(field),
  });

  const base: Extension = [
    ...readingExtensions(),
    highlightActiveLineGutter(),
    foldGutter(),
    highlightActiveLine(),
    search({ top: true }),
    keymap.of([...searchKeymap, ...foldKeymap, ...defaultKeymap]),
    changeGutter(),
    definitions,
    flashField,
    commentExtension(
      () => actions,
      () => askLabel,
    ),
    EditorView.contentAttributes.of({ 'aria-label': 'File contents' }),
    EditorView.updateListener.of((update) => {
      // Keep the stored state current, so a tab switch saves the selection and folds as they are.
      const kept = shown ? states.get(shown) : undefined;
      if (kept) kept.state = update.state;
      if (update.selectionSet || update.docChanged) {
        hasSelection = update.state.selection.ranges.some((range) => !range.empty);
      }
    }),
  ];

  function create(p: string, content: string): Kept {
    const kept: Kept = {
      state: EditorState.create({ doc: content, extensions: [base, language.of([])] }),
      text: content,
      scroll: 0,
    };
    states.set(keyOf(p), kept);
    const description = languageFor(p);
    if (description) {
      const key = keyOf(p);
      void description.load().then((support) => {
        const current = states.get(key);
        if (!current) return;
        const effects = language.reconfigure(support);
        // The state may be on screen by now or not; either way it is the one to change.
        if (shown === key && view) view.dispatch({ effects });
        else current.state = current.state.update({ effects }).state;
      });
    }
    return kept;
  }

  /**
   * Replace a file's text after it changed on disk, touching only the part that changed.
   *
   * The common beginning and end are kept and only the middle is replaced, so the selection, the
   * folds, the comments' tints and the scroll position all map through the edit as they would
   * through typing. Replacing the whole document instead stretches any selection over all of it.
   */
  function reload(key: string, kept: Kept, content: string): void {
    const before = kept.state.doc.toString();
    let from = 0;
    while (
      from < before.length &&
      from < content.length &&
      before[from] === content[from]
    ) {
      from += 1;
    }
    let tail = 0;
    while (
      tail < before.length - from &&
      tail < content.length - from &&
      before[before.length - 1 - tail] === content[content.length - 1 - tail]
    ) {
      tail += 1;
    }
    const changes = {
      from,
      to: before.length - tail,
      insert: content.slice(from, content.length - tail),
    };
    if (shown === key && view) view.dispatch({ changes });
    else kept.state = kept.state.update({ changes }).state;
    kept.text = content;
  }

  function show(key: string, kept: Kept): void {
    if (!view || shown === key) return;
    const previous = shown ? states.get(shown) : undefined;
    if (previous) previous.scroll = view.scrollDOM.scrollTop;
    shown = key;
    view.setState(kept.state);
    hasSelection = kept.state.selection.ranges.some((range) => !range.empty);
    const scroll = kept.scroll;
    requestAnimationFrame(() => {
      if (view && shown === key) view.scrollDOM.scrollTop = scroll;
    });
  }

  onMount(() => {
    if (!host) return;
    view = new EditorView({
      parent: host,
      state: EditorState.create({ extensions: base }),
    });
    return () => {
      view?.destroy();
      view = null;
    };
  });

  // Put the active file on screen, creating or refreshing its state as needed.
  $effect(() => {
    const p = path;
    const content = text;
    if (!p || content === null) return;
    untrack(() => {
      const key = keyOf(p);
      let kept = states.get(key);
      if (!kept) kept = create(p, content);
      else if (kept.text !== content) reload(key, kept, content);
      show(key, kept);
      view?.requestMeasure();
    });
  });

  // Give the file on screen its change markers. After the effect above, so the state is shown.
  $effect(() => {
    const p = path;
    const marks = hunks;
    if (!p || text === null) return;
    untrack(() => {
      const key = keyOf(p);
      const kept = states.get(key);
      if (!kept || given.get(key) === marks) return;
      given.set(key, marks);
      const effects = setHunks.of(marks);
      if (shown === key && view) view.dispatch({ effects });
      else kept.state = kept.state.update({ effects }).state;
    });
  });

  // Give the file on screen its comments, and the box for a new one when that is open here.
  const commentsGiven = new Map<string, CommentInput>();
  $effect(() => {
    const p = path;
    const input: CommentInput = { comments, composing };
    if (!p || text === null) return;
    untrack(() => {
      const key = keyOf(p);
      const kept = states.get(key);
      const last = commentsGiven.get(key);
      if (
        !kept ||
        (last && last.comments === input.comments && last.composing === input.composing)
      ) {
        return;
      }
      commentsGiven.set(key, input);
      const effects = setComments.of(input);
      if (shown === key && view) view.dispatch({ effects });
      else kept.state = kept.state.update({ effects }).state;
    });
  });

  /** Right-click: the same two offers as the selection's, and the ways to copy where you are. */
  function onContextMenu(event: MouseEvent): void {
    const editor = view;
    const p = path;
    if (!editor || !p || text === null) return;
    if ((event.target as Element | null)?.closest?.('.c-code-editor__comment')) return;
    event.preventDefault();
    if (editor.state.selection.main.empty) {
      const at = editor.posAtCoords({ x: event.clientX, y: event.clientY });
      if (at !== null) editor.dispatch({ selection: { anchor: at } });
    }
    const { start, end } = selectedLines(editor.state);
    const where = reference(p, start, end);
    const copy = (value: string) =>
      void navigator.clipboard.writeText(value).catch(() => {});
    void popUp([
      item('Add Comment…', () => actions.compose(start, end)),
      item(
        askLabel ? `Ask ${askLabel} About ${rangeLabel(start, end)}` : 'Ask an Agent',
        () => actions.ask(start, end, linesText(editor.state, start, end) ?? ''),
        askLabel !== null,
      ),
      separator,
      item(`Copy ${where}`, () => copy(where)),
      item('Copy Path', () => copy(p)),
    ]);
  }

  // Forget the states of tabs that were closed.
  $effect(() => {
    const open = new Set((code.tabs[worktreeId] ?? []).map(keyOf));
    for (const key of states.keys()) {
      if (key.startsWith(`${worktreeId}\0`) && !open.has(key)) {
        states.delete(key);
        given.delete(key);
        commentsGiven.delete(key);
      }
    }
  });

  // Go where search or a link asked, once the file it names is the one on screen.
  $effect(() => {
    const reveal = code.reveal;
    const ready = text !== null;
    if (!reveal || !ready || reveal.id === lastReveal) return;
    if (reveal.worktreeId !== worktreeId || reveal.path !== path) return;
    untrack(() => {
      const editor = view;
      if (!editor || shown !== keyOf(reveal.path)) return;
      lastReveal = reveal.id;
      const doc = editor.state.doc;
      const line = doc.line(Math.min(Math.max(reveal.line, 1), doc.lines));
      const anchor = Math.min(line.from + (reveal.from ?? 0), line.to);
      const head = Math.min(line.from + (reveal.to ?? reveal.from ?? 0), line.to);
      editor.dispatch({
        selection: { anchor, head },
        effects: [EditorView.scrollIntoView(anchor, { y: 'center' }), flash.of(line.from)],
      });
      editor.focus();
      setTimeout(() => {
        if (view === editor) editor.dispatch({ effects: flash.of(null) });
      }, 1200);
    });
  });

  /**
   * Move to the next or previous change, wrapping, and say which one of how many it is.
   *
   * Changes are counted by hunk, not by line, so a block of ten edited lines is one step.
   */
  export function step(direction: 1 | -1): { index: number; count: number } | null {
    const editor = view;
    const marks = hunks;
    if (!editor || !marks || marks.length === 0) return null;
    const starts = hunkStarts(marks);
    const here = editor.state.doc.lineAt(editor.state.selection.main.head).number;
    let index =
      direction === 1
        ? starts.findIndex((line) => line > here)
        : starts.length - 1 - [...starts].reverse().findIndex((line) => line < here);
    if (index < 0 || index >= starts.length)
      index = direction === 1 ? 0 : starts.length - 1;
    const doc = editor.state.doc;
    const line = doc.line(Math.min(starts[index] ?? 1, doc.lines));
    editor.dispatch({
      selection: { anchor: line.from },
      effects: [EditorView.scrollIntoView(line.from, { y: 'center' }), flash.of(line.from)],
    });
    editor.focus();
    setTimeout(() => {
      if (view === editor) editor.dispatch({ effects: flash.of(null) });
    }, 1200);
    return { index, count: starts.length };
  }

  /** ⌘F from outside the editor: focus it and open its find panel. */
  export function find(): void {
    if (!view || shown === null) return;
    view.focus();
    openSearchPanel(view);
  }
</script>

<div
  class="c-code-editor c-code-editor--viewer c-code-view__editor"
  class:has-selection={hasSelection}
  class:is-hidden={text === null}
  bind:this={host}
  oncontextmenu={onContextMenu}
  role="presentation"
></div>

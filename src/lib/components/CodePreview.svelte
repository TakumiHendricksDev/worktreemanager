<script lang="ts">
  /**
   * Find in Files' preview: the file around the highlighted result, read-only.
   *
   * Its own small view rather than the Code tab's viewer, because it shows whichever result the
   * arrow keys are on and must not disturb the tabs, their scroll positions or their states. It
   * keeps one state and replaces the document when the result moves to another file.
   */
  import { Compartment, EditorState } from '@codemirror/state';
  import { EditorView, highlightActiveLine } from '@codemirror/view';
  import { onMount, untrack } from 'svelte';

  import { readingExtensions } from '../code-editor';
  import { languageFor } from '../code-languages';

  const {
    path,
    text,
    line,
    from,
    to,
  }: {
    path: string | null;
    /** Null while the file is being read. */
    text: string | null;
    line: number;
    /** The match on that line, in UTF-16 units from the line's start. */
    from: number;
    to: number;
  } = $props();

  const language = new Compartment();
  let host = $state<HTMLDivElement | null>(null);
  let view: EditorView | null = null;
  let shownPath: string | null = null;
  let shownText: string | null = null;
  let generation = 0;

  onMount(() => {
    if (!host) return;
    view = new EditorView({
      parent: host,
      state: EditorState.create({
        extensions: [
          ...readingExtensions(),
          highlightActiveLine(),
          language.of([]),
          EditorView.contentAttributes.of({ 'aria-label': 'Preview' }),
        ],
      }),
    });
    return () => {
      view?.destroy();
      view = null;
    };
  });

  // Another file: replace the document and load its language.
  $effect(() => {
    const p = path;
    const content = text;
    if (!p || content === null) return;
    untrack(() => {
      const editor = view;
      if (!editor || (shownPath === p && shownText === content)) return;
      shownPath = p;
      shownText = content;
      generation += 1;
      const mine = generation;
      editor.dispatch({
        changes: { from: 0, to: editor.state.doc.length, insert: content },
        effects: language.reconfigure([]),
      });
      void languageFor(p)
        ?.load()
        .then((support) => {
          if (view === editor && generation === mine) {
            editor.dispatch({ effects: language.reconfigure(support) });
          }
        });
    });
  });

  // The result on screen: select its match and put it in the middle.
  $effect(() => {
    const target = { line, from, to, text };
    if (target.text === null) return;
    untrack(() => {
      const editor = view;
      if (!editor) return;
      const doc = editor.state.doc;
      const at = doc.line(Math.min(Math.max(target.line, 1), doc.lines));
      const anchor = Math.min(at.from + target.from, at.to);
      const head = Math.min(at.from + target.to, at.to);
      editor.dispatch({
        selection: { anchor, head },
        effects: EditorView.scrollIntoView(anchor, { y: 'center' }),
      });
    });
  });
</script>

<div
  class="c-code-editor c-code-editor--viewer c-find__preview"
  class:is-hidden={text === null}
  bind:this={host}
></div>

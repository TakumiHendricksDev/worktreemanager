<script lang="ts">
  /** PostgreSQL-aware query editor with app-owned value and execution state. */
  import { indentWithTab } from '@codemirror/commands';
  import { PostgreSQL, sql } from '@codemirror/lang-sql';
  import { syntaxHighlighting } from '@codemirror/language';
  import { EditorView, basicSetup } from 'codemirror';
  import { keymap, placeholder } from '@codemirror/view';
  import { classHighlighter } from '@lezer/highlight';
  import { onMount } from 'svelte';

  let {
    value = $bindable(),
    selection = $bindable(''),
    onrun,
  }: {
    value: string;
    selection?: string;
    onrun: (sql: string) => void;
  } = $props();

  let host = $state<HTMLDivElement | null>(null);
  let editorView = $state<EditorView | null>(null);
  /** Any range non-empty — the host's `has-selection`, which stops the active line hiding it. */
  let hasSelection = $state(false);

  /*
   * The one colour decision that cannot live in the stylesheet, and it only names a token.
   *
   * CodeMirror's base theme paints a focused selection with
   * `.ͼ2.cm-focused > .cm-scroller > .cm-selectionLayer .cm-selectionBackground` — five classes,
   * which no selector inside the two-compound-part limit can outrank. So the stylesheet's colour
   * showed only while the editor was unfocused, and a focused selection was the base theme's
   * fixed light lavender — which washes the dark theme's light text out to nearly nothing. A
   * theme's rule ties that specificity and wins, because CodeMirror mounts themes after its base.
   */
  const selectionTheme = EditorView.theme({
    '&.cm-focused > .cm-scroller > .cm-selectionLayer .cm-selectionBackground': {
      background: 'var(--selection)',
    },
  });

  function sqlToRun(view: EditorView): string {
    const range = view.state.selection.main;
    return (
      view.state.sliceDoc(range.from, range.to).trim() || view.state.doc.toString().trim()
    );
  }

  onMount(() => {
    if (!host) return;

    const view = new EditorView({
      parent: host,
      doc: value,
      extensions: [
        // CodeMirror's default map assigns this chord to inserting a blank line. Keymaps are
        // checked in extension order, so the query-console contract has to come before setup.
        keymap.of([
          {
            key: 'Mod-Enter',
            run: (activeView) => {
              onrun(sqlToRun(activeView));
              return true;
            },
          },
          indentWithTab,
        ]),
        basicSetup,
        sql({ dialect: PostgreSQL, upperCaseKeywords: true }),
        // Static token classes keep every colour decision in the global stylesheet instead of
        // letting a JavaScript editor theme become a second visual system. `selectionTheme` is
        // the single exception, and its header says why.
        syntaxHighlighting(classHighlighter),
        selectionTheme,
        placeholder('SELECT * FROM …'),
        EditorView.contentAttributes.of({
          'aria-label': 'SQL query',
          'aria-multiline': 'true',
          spellcheck: 'false',
        }),
        EditorView.updateListener.of((update) => {
          if (update.docChanged) value = update.state.doc.toString();
          if (update.docChanged || update.selectionSet) {
            const range = update.state.selection.main;
            selection = update.state.sliceDoc(range.from, range.to).trim();
            hasSelection = update.state.selection.ranges.some((r) => !r.empty);
          }
        }),
      ],
    });

    editorView = view;
    selection = '';

    return () => {
      editorView = null;
      view.destroy();
    };
  });

  $effect(() => {
    const view = editorView;
    const next = value;
    if (!view || view.state.doc.toString() === next) return;

    view.dispatch({
      changes: { from: 0, to: view.state.doc.length, insert: next },
      selection: { anchor: next.length },
    });
  });
</script>

<div class="c-database__editor" class:has-selection={hasSelection} bind:this={host}></div>

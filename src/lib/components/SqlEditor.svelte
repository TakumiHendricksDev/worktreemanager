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
        // letting a JavaScript editor theme become a second visual system.
        syntaxHighlighting(classHighlighter),
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

<div class="c-database__editor" bind:this={host}></div>

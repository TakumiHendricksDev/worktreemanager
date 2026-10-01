<script lang="ts">
  /** PostgreSQL-aware query editor with app-owned value and execution state. */
  import { indentWithTab } from '@codemirror/commands';
  import { PostgreSQL, sql } from '@codemirror/lang-sql';
  import { syntaxHighlighting } from '@codemirror/language';
  import { EditorView, basicSetup } from 'codemirror';
  import { keymap, placeholder } from '@codemirror/view';
  import { classHighlighter } from '@lezer/highlight';
  import { onMount } from 'svelte';
  import { selectionTheme } from '../code-editor';

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
        // the single exception, and `code-editor.ts` says why.
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

  /**
   * Add a statement below what is already here and select it — SQL arriving from an agent's reply.
   *
   * Appended rather than replacing the editor, because whatever the user had typed is theirs, and
   * a dispatched change is on the undo stack either way. Selected, so the console's Run runs this
   * statement and not everything above it.
   */
  export function append(text: string): void {
    const view = editorView;
    if (!view) return;
    const doc = view.state.doc;
    const existing = doc.toString();
    if (existing.trim() === '') {
      view.dispatch({
        changes: { from: 0, to: doc.length, insert: text },
        selection: { anchor: 0, head: text.length },
        scrollIntoView: true,
      });
    } else {
      const gap = existing.endsWith('\n\n') ? '' : existing.endsWith('\n') ? '\n' : '\n\n';
      const from = doc.length + gap.length;
      view.dispatch({
        changes: { from: doc.length, insert: gap + text },
        selection: { anchor: from, head: from + text.length },
        scrollIntoView: true,
      });
    }
    view.focus();
  }

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

<div
  class="c-database__editor c-code-editor"
  class:has-selection={hasSelection}
  bind:this={host}
></div>

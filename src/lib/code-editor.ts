/**
 * What every CodeMirror editor in the app shares: the SQL console's and the Code tab's.
 *
 * Their look lives in `styles/components/_code-editor.scss`. This module holds only what cannot,
 * so that the two editors cannot drift apart on it.
 */

import { syntaxTree } from '@codemirror/language';
import {
  Decoration,
  EditorView,
  MatchDecorator,
  ViewPlugin,
  type DecorationSet,
  type ViewUpdate,
} from '@codemirror/view';
import { tagHighlighter, tags as t } from '@lezer/highlight';

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
export const selectionTheme = EditorView.theme({
  '&.cm-focused > .cm-scroller > .cm-selectionLayer .cm-selectionBackground': {
    background: 'var(--selection)',
  },
});

/**
 * The finer token classes `classHighlighter` does not make, for the Code tab's viewer.
 *
 * `classHighlighter` is deliberately coarse — every name is `tok-variableName` — which is right for
 * a SQL console and flat for reading Python: the name a `def` introduces, a class being declared,
 * `this`, an HTML tag and its attributes all look like any other word. These classes are added
 * *alongside* the coarse ones, so the stylesheet can colour the refinement and fall back to the
 * general class for everything else. Colours stay in `_code-editor.scss`, like every other one.
 */
export const detailHighlighter = tagHighlighter([
  { tag: t.function(t.definition(t.variableName)), class: 'tok-fn-def' },
  { tag: t.definition(t.className), class: 'tok-class-def' },
  { tag: [t.function(t.variableName), t.function(t.propertyName)], class: 'tok-fn' },
  { tag: t.self, class: 'tok-self' },
  { tag: t.standard(t.variableName), class: 'tok-builtin' },
  { tag: t.tagName, class: 'tok-tag' },
  { tag: t.attributeName, class: 'tok-attr' },
]);

const selfMark = Decoration.mark({ class: 'tok-self' });

/**
 * `self` and `cls` in Python, marked as members the way PyCharm colours them.
 *
 * The Python grammar tags them as ordinary variable names, so no highlighter can tell them apart.
 * This finds the words and keeps only the ones the parser says are names — which is what stops a
 * `self` inside a string or a comment from being marked.
 */
const selfMatcher = new MatchDecorator({
  regexp: /\b(?:self|cls)\b/g,
  decorate: (add, from, to, _match, view) => {
    if (syntaxTree(view.state).resolveInner(from, 1).name === 'VariableName') {
      add(from, to, selfMark);
    }
  },
});

export const pythonSelf = ViewPlugin.fromClass(
  class {
    marks: DecorationSet;
    constructor(view: EditorView) {
      this.marks = selfMatcher.createDeco(view);
    }
    update(update: ViewUpdate): void {
      // A parse that finished since the last pass can change what is a name, so re-run on it too.
      if (
        update.docChanged ||
        update.viewportChanged ||
        syntaxTree(update.startState) !== syntaxTree(update.state)
      ) {
        this.marks = selfMatcher.createDeco(update.view);
      } else {
        this.marks = selfMatcher.updateDeco(update, this.marks);
      }
    }
  },
  { decorations: (plugin) => plugin.marks },
);

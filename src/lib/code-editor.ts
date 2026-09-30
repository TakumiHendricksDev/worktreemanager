/**
 * What every CodeMirror editor in the app shares: the SQL console's and the Code tab's.
 *
 * Their look lives in `styles/components/_code-editor.scss`. This module holds only what cannot,
 * so that the two editors cannot drift apart on it.
 */

import { EditorView } from '@codemirror/view';

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

/**
 * Which language a file is, for the Code tab's highlighting.
 *
 * # A table here rather than `@codemirror/language-data`
 *
 * That package describes about a hundred languages and pulls in a parser package for every one of
 * them. This app's users read Python, JavaScript and TypeScript, Vue and Svelte, Rust, Go, the
 * markup and config around them, and not much else — so a short table of the ones worth having is
 * smaller, and adding a row is one import.
 *
 * Every `load` is a dynamic `import()` with a fixed specifier, so Vite splits each language into a
 * chunk of its own that loads the first time a file of that kind is opened. Those chunks are
 * same-origin, which the webview's `script-src 'self'` allows; nothing here needs `eval` or WASM.
 *
 * # The legacy modes
 *
 * TOML, shell, Dockerfile, Ruby, INI/properties, diffs and Jinja have no Lezer grammar in the
 * CodeMirror project. `@codemirror/legacy-modes` ports the old CodeMirror 5 tokenizers, which
 * `StreamLanguage` runs. They highlight tokens without building a syntax tree, which is all a
 * reader needs from a config file.
 */

import { LanguageDescription, LanguageSupport, StreamLanguage } from '@codemirror/language';
import type { StreamParser } from '@codemirror/language';

import { pythonSelf } from './code-editor';

function legacy(parser: Promise<StreamParser<unknown>>): Promise<LanguageSupport> {
  return parser.then((p) => new LanguageSupport(StreamLanguage.define(p)));
}

export const languages: LanguageDescription[] = [
  LanguageDescription.of({
    name: 'Python',
    extensions: ['py', 'pyi', 'pyw'],
    // With `self` and `cls` marked, which the grammar alone cannot tell from any other name.
    load: () =>
      import('@codemirror/lang-python').then((m) => {
        const python = m.python();
        return new LanguageSupport(python.language, [python.support, pythonSelf]);
      }),
  }),
  LanguageDescription.of({
    name: 'TypeScript',
    extensions: ['ts', 'mts', 'cts'],
    load: () =>
      import('@codemirror/lang-javascript').then((m) => m.javascript({ typescript: true })),
  }),
  LanguageDescription.of({
    name: 'TSX',
    extensions: ['tsx'],
    load: () =>
      import('@codemirror/lang-javascript').then((m) =>
        m.javascript({ typescript: true, jsx: true }),
      ),
  }),
  LanguageDescription.of({
    name: 'JavaScript',
    extensions: ['js', 'mjs', 'cjs', 'jsx'],
    load: () =>
      import('@codemirror/lang-javascript').then((m) => m.javascript({ jsx: true })),
  }),
  LanguageDescription.of({
    name: 'Vue',
    extensions: ['vue'],
    load: () => import('@codemirror/lang-vue').then((m) => m.vue()),
  }),
  // A Svelte component is HTML with a `<script>` and a `<style>`, which is exactly what the HTML
  // grammar nests, so its templates and scripts both highlight without a Svelte parser.
  LanguageDescription.of({
    name: 'HTML',
    extensions: ['html', 'htm', 'svelte', 'jinja', 'j2'],
    load: () => import('@codemirror/lang-html').then((m) => m.html()),
  }),
  LanguageDescription.of({
    name: 'CSS',
    extensions: ['css', 'less'],
    load: () => import('@codemirror/lang-css').then((m) => m.css()),
  }),
  LanguageDescription.of({
    name: 'SCSS',
    extensions: ['scss'],
    load: () => import('@codemirror/lang-sass').then((m) => m.sass()),
  }),
  LanguageDescription.of({
    name: 'Sass',
    extensions: ['sass'],
    load: () => import('@codemirror/lang-sass').then((m) => m.sass({ indented: true })),
  }),
  LanguageDescription.of({
    name: 'JSON',
    extensions: ['json', 'jsonc', 'json5', 'webmanifest'],
    filename: /^\.(babelrc|eslintrc|prettierrc|swcrc)$/,
    load: () => import('@codemirror/lang-json').then((m) => m.json()),
  }),
  LanguageDescription.of({
    name: 'Markdown',
    extensions: ['md', 'markdown', 'mdx'],
    // Fenced code in a README highlights in its own language, from this same table.
    load: () =>
      import('@codemirror/lang-markdown').then((m) =>
        m.markdown({ codeLanguages: languages }),
      ),
  }),
  LanguageDescription.of({
    name: 'Rust',
    extensions: ['rs'],
    load: () => import('@codemirror/lang-rust').then((m) => m.rust()),
  }),
  LanguageDescription.of({
    name: 'Go',
    extensions: ['go'],
    load: () => import('@codemirror/lang-go').then((m) => m.go()),
  }),
  LanguageDescription.of({
    name: 'YAML',
    extensions: ['yml', 'yaml'],
    load: () => import('@codemirror/lang-yaml').then((m) => m.yaml()),
  }),
  LanguageDescription.of({
    name: 'SQL',
    extensions: ['sql'],
    load: () =>
      import('@codemirror/lang-sql').then((m) => m.sql({ dialect: m.PostgreSQL })),
  }),
  LanguageDescription.of({
    name: 'XML',
    extensions: ['xml', 'svg', 'plist', 'xsd', 'xsl'],
    load: () => import('@codemirror/lang-xml').then((m) => m.xml()),
  }),
  LanguageDescription.of({
    name: 'TOML',
    extensions: ['toml'],
    filename: /^(Cargo\.lock|Pipfile|poetry\.lock)$/,
    load: () => legacy(import('@codemirror/legacy-modes/mode/toml').then((m) => m.toml)),
  }),
  LanguageDescription.of({
    name: 'Shell',
    extensions: ['sh', 'bash', 'zsh', 'fish'],
    filename: /^(\.(bash|zsh)(rc|_profile|env)|\.profile|justfile|Justfile|\.envrc)$/,
    load: () => legacy(import('@codemirror/legacy-modes/mode/shell').then((m) => m.shell)),
  }),
  LanguageDescription.of({
    name: 'Dockerfile',
    filename: /^(Dockerfile|Containerfile)(\..+)?$/,
    extensions: ['dockerfile'],
    load: () =>
      legacy(import('@codemirror/legacy-modes/mode/dockerfile').then((m) => m.dockerFile)),
  }),
  LanguageDescription.of({
    name: 'Ruby',
    extensions: ['rb', 'rake', 'gemspec'],
    filename: /^(Gemfile|Rakefile|Brewfile|Podfile)$/,
    load: () => legacy(import('@codemirror/legacy-modes/mode/ruby').then((m) => m.ruby)),
  }),
  LanguageDescription.of({
    name: 'Properties',
    extensions: ['ini', 'cfg', 'conf', 'properties', 'env', 'editorconfig'],
    filename: /^(\.env(\..+)?|\.gitconfig|\.npmrc|setup\.cfg|tox\.ini)$/,
    load: () =>
      legacy(import('@codemirror/legacy-modes/mode/properties').then((m) => m.properties)),
  }),
  LanguageDescription.of({
    name: 'Diff',
    extensions: ['diff', 'patch'],
    load: () => legacy(import('@codemirror/legacy-modes/mode/diff').then((m) => m.diff)),
  }),
];

/** The language a path is written in, from its file name, or null for plain text. */
export function languageFor(path: string): LanguageDescription | null {
  const name = path.slice(path.lastIndexOf('/') + 1);
  return LanguageDescription.matchFilename(languages, name);
}

/**
 * A short name for a fenced block's info string, when an excerpt is sent to an agent.
 *
 * The language's own name lowercased, which is what Markdown renderers and models both read:
 * `python`, `typescript`, `rust`.
 */
export function fenceFor(path: string): string {
  return languageFor(path)?.name.toLowerCase() ?? '';
}

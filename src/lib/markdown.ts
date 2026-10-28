/**
 * A small markdown reader for agent output.
 *
 * # Why this exists rather than a dependency
 *
 * The obvious answer is `marked` plus `DOMPurify`, and the obvious answer costs two dependencies
 * and the codebase's first `{@html}`. This produces a *tree*, which `Markdown.svelte` renders as
 * real elements — so text an agent emits can never become markup, whatever it contains. There is
 * nothing to sanitize because nothing is ever parsed as HTML. That property is worth more here
 * than table support, because the untrusted end of this pipe is a language model quoting whatever
 * it just read out of a file.
 *
 * # It parses a prefix, not a document
 *
 * Every call sees a message that is still arriving: `MessageDelta`s land many times a second and
 * the transcript re-renders each time. So a half-written fence, a `**` with no partner and a list
 * whose next item has not been typed yet are all *normal input*, not errors. Each is treated as
 * valid-so-far — an unterminated fence runs to the end of what exists, an unmatched delimiter is
 * literal text — which is what stops a reply flickering between formatted and raw as it streams.
 *
 * # What it deliberately does not do
 *
 * Footnotes, reference links, setext headings, HTML blocks. None has shown up in a reply often
 * enough to pay for itself, and the failure mode is that the source shows through as text.
 */

export type Span =
  | { kind: 'text'; text: string }
  | { kind: 'code'; text: string }
  | { kind: 'strong'; spans: Span[] }
  | { kind: 'em'; spans: Span[] }
  | { kind: 'strike'; spans: Span[] }
  | { kind: 'link'; href: string; spans: Span[] }
  /** `[label](path)`: a file in the worktree, never an anchor. See `FILE_TARGET`. */
  | { kind: 'file'; target: string; spans: Span[] };

/** One bullet: its own line, plus anything indented underneath it. */
export interface ListItem {
  spans: Span[];
  children: Block[];
}

export type TableAlignment = 'left' | 'center' | 'right' | null;

export type Block =
  | { kind: 'paragraph'; spans: Span[] }
  | { kind: 'heading'; level: number; spans: Span[] }
  | { kind: 'code'; lang: string | null; text: string; raw: string; complete: boolean }
  | { kind: 'list'; ordered: boolean; start: number; items: ListItem[] }
  | { kind: 'table'; header: Span[][]; alignments: TableAlignment[]; rows: Span[][][] }
  | { kind: 'quote'; blocks: Block[] }
  | { kind: 'rule' };

const FENCE = /^(\s*)(`{3,}|~{3,})\s*([^\s`]*)/;
const HEADING = /^ {0,3}(#{1,6})\s+(.*)$/;
const RULE = /^ {0,3}([-*_])[ \t]*(?:\1[ \t]*){2,}$/;
const QUOTE = /^ {0,3}>[ \t]?/;
const BULLET = /^(\s*)([-*+])[ \t]+(.*)$/;
const ORDERED = /^(\s*)(\d{1,9})[.)][ \t]+(.*)$/;

/**
 * Link schemes that may reach the DOM.
 *
 * Matched to what `open_url` in Rust will actually accept, so a rendered link is one that works
 * rather than one that fails silently on click. Anything else — `javascript:`, `data:` — falls back
 * to plain text: an anchor is never built, so there is no href to neutralise later. A path is the
 * one other target with a meaning here, and it gets a span of its own; see `FILE_TARGET`.
 */
const SAFE_HREF = /^https?:\/\/\S/i;

/**
 * A link target that names a file rather than a page: `apps/views.py`, `views.py:42`, `/abs/x.md`.
 *
 * Models link the files they cite this way constantly — about one Claude reply in eight on one
 * machine over a month — and before this every one of them showed as the raw `[label](target)`
 * source, because the only targets accepted were web pages. It becomes a `file` span, which
 * `Markdown.svelte` turns into the Code tab's button when the path resolves in the worktree and
 * plain text when it does not; it is never an `href`, so the safety argument at `SAFE_HREF` is
 * untouched.
 *
 * Anything that looks like a scheme is refused, with one exception: a colon followed by a digit is
 * a line number (`views.py:42`), not a scheme. Only the start is checked here; whether a space may
 * appear depends on how the target was written, which `readLink` knows.
 */
const FILE_TARGET = /^(?![a-z][a-z0-9+.-]*:(?!\d))[^\s#?]/i;

/** A bare URL in running text. Trailing punctuation is left to the sentence, not eaten by the link. */
const BARE_URL = /^https?:\/\/[^\s<>[\]()]*[^\s<>[\]().,;:!?'"]/i;

export function parse(source: string): Block[] {
  return blocks(source.split('\n'));
}

function blocks(lines: string[]): Block[] {
  const out: Block[] = [];
  let i = 0;

  while (i < lines.length) {
    const line = lines[i] ?? '';

    if (line.trim() === '') {
      i += 1;
      continue;
    }

    const fence = FENCE.exec(line);
    if (fence) {
      const [, indent = '', marker = '```', lang = ''] = fence;
      const body: string[] = [];
      i += 1;
      // No closer is the ordinary case mid-stream, so running out of lines ends the block just as a
      // closing fence would. Anything else would make every code block flash as literal backticks
      // between its first line and its last.
      while (
        i < lines.length &&
        !isFenceEnd(lines[i] ?? '', marker[0] ?? '`', marker.length)
      ) {
        body.push(deindent(lines[i] ?? '', indent.length));
        i += 1;
      }
      const complete = i < lines.length;
      if (complete) i += 1;
      // Trailing blank lines trimmed: mid-stream the last line is always the empty tail of the
      // split, and a code block that grows a blank line at the bottom on every keystroke jitters.
      out.push({
        kind: 'code',
        lang: lang || null,
        text: body.join('\n').replace(/\r\n?/g, '\n').replace(/\s+$/, ''),
        raw: body.join('\n') + (complete && body.length > 0 ? '\n' : ''),
        complete,
      });
      continue;
    }

    const heading = HEADING.exec(line);
    if (heading) {
      out.push({
        kind: 'heading',
        level: (heading[1] ?? '#').length,
        // Closing hashes are decoration in the source and never content.
        spans: inline((heading[2] ?? '').replace(/\s+#+\s*$/, '')),
      });
      i += 1;
      continue;
    }

    if (RULE.test(line)) {
      out.push({ kind: 'rule' });
      i += 1;
      continue;
    }

    const table = readTable(lines, i);
    if (table) {
      out.push(table[0]);
      i = table[1];
      continue;
    }

    if (QUOTE.test(line)) {
      const body: string[] = [];
      while (
        i < lines.length &&
        (QUOTE.test(lines[i] ?? '') || (lines[i] ?? '').trim() !== '')
      ) {
        const quoted = lines[i] ?? '';
        if (!QUOTE.test(quoted) && body.length === 0) break;
        body.push(quoted.replace(QUOTE, ''));
        i += 1;
      }
      out.push({ kind: 'quote', blocks: blocks(body) });
      continue;
    }

    if (BULLET.test(line) || ORDERED.test(line)) {
      const [list, next] = readList(lines, i);
      out.push(list);
      i = next;
      continue;
    }

    // A paragraph runs until a blank line or the start of some other block.
    const text: string[] = [];
    while (i < lines.length) {
      const run = lines[i] ?? '';
      if (run.trim() === '' || startsBlock(run) || (text.length > 0 && readTable(lines, i)))
        break;
      text.push(run.trim());
      i += 1;
    }
    const spans = inline(text.join('\n'));
    out.push(sectionLabel(spans) ?? { kind: 'paragraph', spans });
  }

  return out;
}

/** The longest bold line still read as a title rather than as an emphasised sentence. */
const LABEL_CHARS = 60;

/**
 * A paragraph that is only a bold label — `**What changed:**` — as the heading it is standing in for.
 *
 * Models title a section two ways, and on one machine over a month Claude wrote the bold label in
 * 1,108 replies against `## heading` in 162. Rendered literally, the same kind of title had two
 * looks, and the label's lacked the heading's air above it, so a reply's sections ran together.
 *
 * Narrow on purpose. The whole paragraph has to be one bold run, optionally followed by a colon;
 * short; one line; and not ending a sentence. `**Done.**` is a sentence said loudly and
 * `**Note:** the rest…` is a lead-in, and both stay paragraphs.
 */
function sectionLabel(spans: Span[]): Block | null {
  const [first, ...rest] = spans;
  if (first?.kind !== 'strong') return null;
  if (!rest.every((span) => span.kind === 'text' && /^\s*:?\s*$/.test(span.text)))
    return null;

  const words = plain(first.spans).trim();
  if (words.replace(/:$/, '').trim() === '') return null;
  if (words.length > LABEL_CHARS || words.includes('\n')) return null;
  if (/[.!?]$/.test(words)) return null;

  // The colon belongs to the label's sentence, not to a heading, which says "this section" alone.
  const last = first.spans.at(-1);
  const spansOut =
    last?.kind === 'text'
      ? [
          ...first.spans.slice(0, -1),
          { kind: 'text' as const, text: last.text.replace(/\s*:\s*$/, '') },
        ]
      : first.spans;
  return { kind: 'heading', level: 2, spans: spansOut };
}

/** The visible text of a run of spans. */
function plain(spans: Span[]): string {
  return spans
    .map((span) =>
      span.kind === 'text' || span.kind === 'code' ? span.text : plain(span.spans),
    )
    .join('');
}

/** Read a GitHub-style table headed by a row and its delimiter row. */
function readTable(lines: string[], from: number): [Block, number] | null {
  const header = splitTableRow(lines[from] ?? '');
  const delimiter = splitTableRow(lines[from + 1] ?? '');
  if (!header || !delimiter || header.length !== delimiter.length) return null;

  const alignments: TableAlignment[] = [];
  for (const cell of delimiter) {
    const alignment = tableAlignment(cell);
    if (alignment === undefined) return null;
    alignments.push(alignment);
  }

  const rows: Span[][][] = [];
  let i = from + 2;
  while (i < lines.length && (lines[i] ?? '').trim() !== '') {
    const cells = splitTableRow(lines[i] ?? '');
    if (!cells) break;
    // GitHub tables pad short rows and ignore surplus cells. Doing that here keeps every row on the
    // header's grid instead of making a streaming or slightly uneven model response malformed.
    rows.push(header.map((_, column) => inline(cells[column] ?? '')));
    i += 1;
  }

  return [
    {
      kind: 'table',
      header: header.map(inline),
      alignments,
      rows,
    },
    i,
  ];
}

/**
 * Split pipes that belong to the table rather than escaped pipes or pipes inside inline code.
 *
 * The backslashes and backticks remain in the cells for `inline` to interpret. Keeping one parser
 * responsible for inline syntax is what makes a table cell behave exactly like ordinary prose.
 */
function splitTableRow(line: string): string[] | null {
  const source = line.trim();
  const cells: string[] = [];
  let cell = '';
  let sawDivider = false;
  let firstWasDivider = false;
  let lastWasDivider = false;
  let i = 0;

  while (i < source.length) {
    const char = source[i] ?? '';

    if (char === '\\' && i + 1 < source.length) {
      cell += source.slice(i, i + 2);
      lastWasDivider = false;
      i += 2;
      continue;
    }

    if (char === '`') {
      const marker = /^`+/.exec(source.slice(i))?.[0] ?? '`';
      const close = source.indexOf(marker, i + marker.length);
      if (close !== -1) {
        cell += source.slice(i, close + marker.length);
        lastWasDivider = false;
        i = close + marker.length;
        continue;
      }
    }

    if (char === '|') {
      if (i === 0) firstWasDivider = true;
      cells.push(cell.trim());
      cell = '';
      sawDivider = true;
      lastWasDivider = true;
      i += 1;
      continue;
    }

    cell += char;
    lastWasDivider = false;
    i += 1;
  }

  if (!sawDivider) return null;
  cells.push(cell.trim());
  if (firstWasDivider) cells.shift();
  if (lastWasDivider) cells.pop();
  return cells;
}

/** A delimiter cell such as `---`, `:---` or `---:`. */
function tableAlignment(cell: string): TableAlignment | undefined {
  const delimiter = cell.trim();
  if (!/^:?-{3,}:?$/.test(delimiter)) return undefined;
  const left = delimiter.startsWith(':');
  const right = delimiter.endsWith(':');
  if (left && right) return 'center';
  if (right) return 'right';
  if (left) return 'left';
  return null;
}

function isFenceEnd(line: string, char: string, length: number): boolean {
  const end = new RegExp(`^\\s*${char === '`' ? '`' : '~'}{${length},}\\s*$`);
  return end.test(line);
}

function deindent(line: string, by: number): string {
  let n = 0;
  while (n < by && (line[n] === ' ' || line[n] === '\t')) n += 1;
  return line.slice(n);
}

function startsBlock(line: string): boolean {
  return (
    FENCE.test(line) ||
    HEADING.test(line) ||
    RULE.test(line) ||
    QUOTE.test(line) ||
    BULLET.test(line) ||
    ORDERED.test(line)
  );
}

/** Read one run of list items. Returns the block and the line after it. */
function readList(lines: string[], from: number): [Block, number] {
  const first = BULLET.exec(lines[from] ?? '') ?? ORDERED.exec(lines[from] ?? '');
  const ordered = BULLET.exec(lines[from] ?? '') === null;
  const start = ordered ? Number.parseInt(first?.[2] ?? '1', 10) : 1;
  const items: ListItem[] = [];

  let i = from;
  while (i < lines.length) {
    /*
     * Blank lines between items keep the list together.
     *
     * A "loose" list — one with air between the bullets — is still one list. Ending it at the first
     * blank line produced a fresh `<ol>` per item, and since each starts its own counter the reader
     * saw "1." three times down the page.
     */
    let at = i;
    while (at < lines.length && (lines[at] ?? '').trim() === '') at += 1;

    const line = lines[at] ?? '';
    const match = BULLET.exec(line) ?? ORDERED.exec(line);
    // A different marker kind starts a different list, which is what keeps a numbered list under a
    // bulleted one from being swallowed into it.
    if (!match || (BULLET.exec(line) === null) !== ordered) break;
    i = at;

    const indent = (match[1] ?? '').length;
    const own = [match[3] ?? ''];
    const nested: string[] = [];
    /*
     * Whether a blank line has been seen inside this item, which is what divides its two halves.
     *
     * Before one, a plain line is a continuation of the item's own sentence however it is indented —
     * `- first line` / `  continued` is one bullet reading "first line continued", not a bullet with
     * a paragraph hanging under it. After one, indented content is a child block: a nested list, a
     * second paragraph, a fenced example.
     */
    let broken = false;
    i += 1;

    while (i < lines.length) {
      const run = lines[i] ?? '';
      if (run.trim() === '') {
        const after = lines[i + 1] ?? '';
        if (after.trim() === '' || leadingSpaces(after) <= indent) break;
        broken = true;
        if (nested.length > 0) nested.push('');
        i += 1;
        continue;
      }
      const item = BULLET.exec(run) ?? ORDERED.exec(run);
      if (item && (item[1] ?? '').length <= indent) break;

      const child = leadingSpaces(run) > indent;
      /*
       * A fenced block under an item belongs to it through its closing fence, whatever its lines
       * look like.
       *
       * The check below sees one line at a time, so it took the opening fence as a child and then
       * met the command inside it — a plain line, no blank before it — and ended the item there. The
       * command fell out as a paragraph, the *closing* fence opened a new block, and everything after
       * it, the rest of the reply, rendered as code. Twelve replies on one machine in a month.
       */
      if (child && FENCE.test(run)) {
        const marker = FENCE.exec(run)?.[2] ?? '```';
        nested.push(deindent(run, indent + 2));
        i += 1;
        while (i < lines.length) {
          const body = lines[i] ?? '';
          nested.push(deindent(body, indent + 2));
          i += 1;
          if (isFenceEnd(body, marker[0] ?? '`', marker.length)) break;
        }
        continue;
      }
      // Once a child block has started, an indented line goes on with it, as the sentence after a
      // fenced example does. Ending the item there put that sentence outside its list.
      if (child && (broken || startsBlock(run) || nested.length > 0)) {
        nested.push(deindent(run, indent + 2));
      } else if (startsBlock(run)) {
        break;
      } else if (nested.length === 0) {
        own.push(run.trim());
      } else {
        break;
      }
      i += 1;
    }

    items.push({
      spans: inline(own.join('\n')),
      children: nested.length ? blocks(nested) : [],
    });
  }

  return [{ kind: 'list', ordered, start, items }, i];
}

function leadingSpaces(line: string): number {
  return (/^\s*/.exec(line)?.[0] ?? '').length;
}

/**
 * Inline spans.
 *
 * Left to right with a plain-text buffer: a delimiter that finds no partner is flushed as the
 * literal characters it is made of, which is both correct for `2 * 3 * 4` and what makes a
 * half-typed `**bo` render as itself rather than disappearing until its closer arrives.
 */
function inline(source: string): Span[] {
  const out: Span[] = [];
  let text = '';
  let i = 0;

  const flush = () => {
    if (text) out.push({ kind: 'text', text });
    text = '';
  };

  while (i < source.length) {
    const char = source[i] ?? '';

    if (char === '\\' && i + 1 < source.length) {
      text += source[i + 1];
      i += 2;
      continue;
    }

    if (char === '`') {
      const run = /^`+/.exec(source.slice(i))?.[0] ?? '`';
      const close = source.indexOf(run, i + run.length);
      if (close !== -1) {
        flush();
        // A single leading and trailing space is padding that lets a span hold a backtick.
        out.push({
          kind: 'code',
          text: source.slice(i + run.length, close).replace(/^ | $/g, ''),
        });
        i = close + run.length;
        continue;
      }
    }

    if (char === '[') {
      const link = readLink(source, i);
      if (link) {
        flush();
        out.push(...link.spans);
        i = link.next;
        continue;
      }
    }

    if ((char === 'h' || char === 'H') && (i === 0 || /[\s(]/.test(source[i - 1] ?? ' '))) {
      const bare = BARE_URL.exec(source.slice(i))?.[0];
      if (bare) {
        flush();
        out.push({ kind: 'link', href: bare, spans: [{ kind: 'text', text: bare }] });
        i += bare.length;
        continue;
      }
    }

    /*
     * `*` only. Underscore never marks emphasis here, and that is a deliberate departure.
     *
     * CommonMark says `__init__` is strong and `_x_` is emphasis, and by the letter of it that is
     * right. But the text flowing through this parser is a model discussing source code, where
     * `__init__`, `__all__`, `MAX_SIZE` and `some_var_name` appear constantly and `_italic_` almost
     * never — every model that writes markdown for a chat window reaches for `*` and `**`. Honouring
     * the underscore rule turned dunder names into bold and gained nothing in exchange.
     *
     * An intraword guard was tried first and is not sufficient: `__init__` is flanked by spaces, so
     * it satisfies every flanking rule there is and still must not be bold.
     */
    const emphasis =
      wrap(source, i, '~~', 'strike') ??
      wrap(source, i, '**', 'strong') ??
      wrap(source, i, '*', 'em');
    if (emphasis) {
      flush();
      out.push(emphasis.span);
      i = emphasis.next;
      continue;
    }

    text += char;
    i += 1;
  }

  flush();
  return out;
}

/** `[label](href)` as the spans it stands for, or null when it is just a bracket. */
function readLink(source: string, at: number): { spans: Span[]; next: number } | null {
  let depth = 0;
  let close = -1;
  for (let i = at; i < source.length; i += 1) {
    if (source[i] === '\\') {
      i += 1;
      continue;
    }
    if (source[i] === '[') depth += 1;
    if (source[i] === ']') {
      depth -= 1;
      if (depth === 0) {
        close = i;
        break;
      }
    }
  }
  if (close === -1 || source[close + 1] !== '(') return null;

  const end = source.indexOf(')', close + 2);
  if (end === -1) return null;

  const written = source.slice(close + 2, end).trim();
  const label = source.slice(at + 1, close);
  const next = end + 1;
  // `<…>` is how CommonMark writes a target with spaces in it, and models use it for exactly that:
  // a note under a folder with a space in its name.
  const angled = /^<([^<>]*)>$/.exec(written);
  const href = angled ? (angled[1] ?? '').trim() : written;

  if (SAFE_HREF.test(href)) {
    return { spans: [{ kind: 'link', href, spans: inline(label) }], next };
  }
  if (label.trim() === '') return null;
  if (FILE_TARGET.test(href) && (angled !== null || !/\s/.test(href))) {
    return { spans: [{ kind: 'file', target: href, spans: inline(label) }], next };
  }
  /*
   * A link into a page wtm does not have: `#local_…`, or a bare `#`.
   *
   * Models carry these over from surfaces where they meant something. Here there is nowhere for one
   * to go, and the brackets and the id were what made the sentence around it look broken, so it is
   * the words alone. About a hundred of these in a month of one machine's replies.
   */
  if (href.startsWith('#')) return { spans: inline(label), next };

  // Not a scheme the app can open, so no anchor is built at all — the source shows through as the
  // text it is. See `SAFE_HREF`.
  return null;
}

/** A paired delimiter such as `**`, or null when this position does not open one. */
function wrap(
  source: string,
  at: number,
  delim: string,
  kind: 'strong' | 'em' | 'strike',
): { span: Span; next: number } | null {
  if (!source.startsWith(delim, at)) return null;
  // A delimiter needs something to wrap, and an opener cannot be followed by a space.
  if (/\s/.test(source[at + delim.length] ?? ' ')) return null;

  let i = at + delim.length;
  while (i < source.length) {
    if (source[i] === '\\') {
      i += 2;
      continue;
    }
    if (source.startsWith(delim, i) && !/\s/.test(source[i - 1] ?? ' ')) {
      /*
       * A closer touching its own opener is not a closer.
       *
       * Without this, the `*` pass reads the second asterisk of a half-typed `**bold` as the
       * partner of the first and emits an empty `<em>` — so the two characters the user just typed
       * vanish until the closing pair arrives. The `**` pass has already declined by then, which is
       * exactly the moment this case shows up.
       */
      if (i === at + delim.length) return null;
      return {
        span: { kind, spans: inline(source.slice(at + delim.length, i)) } as Span,
        next: i + delim.length,
      };
    }
    i += 1;
  }
  // No partner yet. Literal, so a reply does not flicker as its closer is typed.
  return null;
}

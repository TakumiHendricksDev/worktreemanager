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
 * Tables, footnotes, reference links, setext headings, HTML blocks. None has shown up in a reply
 * often enough to pay for itself, and the failure mode is that the source shows through as text.
 */

export type Span =
  | { kind: 'text'; text: string }
  | { kind: 'code'; text: string }
  | { kind: 'strong'; spans: Span[] }
  | { kind: 'em'; spans: Span[] }
  | { kind: 'strike'; spans: Span[] }
  | { kind: 'link'; href: string; spans: Span[] };

/** One bullet: its own line, plus anything indented underneath it. */
export interface ListItem {
  spans: Span[];
  children: Block[];
}

export type Block =
  | { kind: 'paragraph'; spans: Span[] }
  | { kind: 'heading'; level: number; spans: Span[] }
  | { kind: 'code'; lang: string | null; text: string }
  | { kind: 'list'; ordered: boolean; start: number; items: ListItem[] }
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
 * rather than one that fails silently on click. Anything else — `javascript:`, `data:`, a bare
 * relative path — falls back to plain text: an anchor is never built, so there is no href to
 * neutralise later.
 */
const SAFE_HREF = /^https?:\/\/\S/i;

/** A bare URL in running text. Trailing punctuation is left to the sentence, not eaten by the link. */
const BARE_URL = /^https?:\/\/[^\s<>[\]()]*[^\s<>[\]().,;:!?'"]/i;

export function parse(source: string): Block[] {
  return blocks(source.replace(/\r\n?/g, '\n').split('\n'));
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
      if (i < lines.length) i += 1;
      // Trailing blank lines trimmed: mid-stream the last line is always the empty tail of the
      // split, and a code block that grows a blank line at the bottom on every keystroke jitters.
      out.push({
        kind: 'code',
        lang: lang || null,
        text: body.join('\n').replace(/\s+$/, ''),
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
      if (run.trim() === '' || startsBlock(run)) break;
      text.push(run.trim());
      i += 1;
    }
    out.push({ kind: 'paragraph', spans: inline(text.join('\n')) });
  }

  return out;
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
      if (child && (broken || startsBlock(run))) {
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
        out.push(link.span);
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

/** `[label](href)`, or null when it is just a bracket. */
function readLink(source: string, at: number): { span: Span; next: number } | null {
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

  const href = source.slice(close + 2, end).trim();
  const label = source.slice(at + 1, close);
  // Not a scheme the app can open, so no anchor is built at all — the source shows through as the
  // text it is. See `SAFE_HREF`.
  if (!SAFE_HREF.test(href)) return null;

  return { span: { kind: 'link', href, spans: inline(label) }, next: end + 1 };
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

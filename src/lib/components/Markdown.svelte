<script lang="ts">
  /**
   * Renders what `markdown.ts` parsed, as elements.
   *
   * The tree in, DOM out — there is no string stage and no `{@html}`, so nothing an agent writes
   * can become markup. That is the security property, and it is structural: there is no sanitizer
   * to misconfigure because there is nothing to sanitize.
   *
   * Two recursive snippets rather than a component that imports itself. A nested list inside a
   * blockquote is ordinary markdown, so the renderer has to recurse somewhere; snippets keep it in
   * one file and cost no component instances per span, which matters when a streaming reply
   * re-renders many times a second.
   */
  import { commands } from '../ipc/commands';
  import type { CodeRef } from '../code-links';
  import { parse, type Block, type Span } from '../markdown';
  import { onDestroy } from 'svelte';
  import Button from './ui/Button.svelte';
  import { shellSnippet, type ShellSnippet } from '../shell-script';

  /** How a reply's file references resolve and open. See `code-links.ts`. */
  export interface CodeLink {
    resolve: (text: string) => CodeRef | null;
    open: (ref: CodeRef) => void;
    /** The worktree's absolute path, without a trailing slash, for showing a path relative to it. */
    root: string;
  }

  const {
    source,
    onrunsql,
    onrunshell,
    codeLink,
  }: {
    source: string;
    /**
     * Where a code block's SQL goes to be run — the Database console. Opt-in, because this renders
     * plans, approval requests and side answers too, and only an agent's reply in a pane has a
     * worktree whose database the SQL is about.
     */
    onrunsql?: (sql: string) => void;
    onrunshell?: (snippet: ShellSnippet) => void;
    /**
     * Inline code that names a file in the worktree — `src/app.py:42` — becomes a link that opens it
     * in the Code tab. Opt-in for the same reason as `onrunsql`: only a reply in a pane has a
     * worktree its paths are relative to.
     */
    codeLink?: CodeLink;
  } = $props();

  /** The fence languages an agent tags SQL with. Untagged blocks get no Run: a guess would misfire. */
  const SQL = new Set([
    'sql',
    'postgres',
    'postgresql',
    'pgsql',
    'psql',
    'plpgsql',
    'sqlite',
  ]);

  function runnable(lang: string | null): boolean {
    return onrunsql !== undefined && lang !== null && SQL.has(lang.toLowerCase());
  }

  const blocks = $derived(parse(source));

  /*
   * A link opens in the user's browser, never in the webview.
   *
   * `elements/_links.scss` has said since it was written that "an anchor would need an href the
   * webview cannot navigate to" — following one in place would replace the running app with a web
   * page and there is no way back. So the anchor is real, for hover and right-click-copy, and the
   * navigation is the one thing that is intercepted. `open_url` re-checks the scheme in Rust; the
   * parser has already refused anything that is not http(s).
   */
  function open(event: MouseEvent, href: string) {
    event.preventDefault();
    void commands.openUrl(href).catch(() => {});
  }

  /*
   * The block just copied, by its text.
   *
   * Not by its index: an index counts within one list of blocks, and a code block inside a list item
   * sits in a list of its own, so the first block of the reply and the first block of an item were
   * both "block 0" and both said Copied.
   */
  let copiedBlock = $state<string | null>(null);
  let copyTimer: ReturnType<typeof setTimeout> | undefined;

  async function copyBlock(text: string) {
    try {
      await navigator.clipboard.writeText(text);
      copiedBlock = text;
      clearTimeout(copyTimer);
      copyTimer = setTimeout(() => (copiedBlock = null), 1400);
    } catch {
      /* Clipboard access can be denied. */
    }
  }

  onDestroy(() => clearTimeout(copyTimer));

  /**
   * A file link's label as one token, when it is one: `storage.py`, `apps/x.py:12`, or either in
   * backticks.
   *
   * That label is a name, so it is drawn as the code an inline path already is, and a link written
   * one way looks like a path written the other. A label with words in it is a phrase that happens
   * to point at a file, and stays prose.
   */
  function pathLabel(list: Span[]): string | null {
    const [only, ...rest] = list;
    if (!only || rest.length > 0 || (only.kind !== 'text' && only.kind !== 'code'))
      return null;
    const text = only.text.trim();
    return text === '' || /\s/.test(text) ? null : text;
  }
</script>

<!--
  Kept on one line, and that is not a formatting accident.

  Svelte turns the whitespace between template tags into text nodes, so breaking this across lines
  puts a space on either side of every span — `**no**where` would render as "no where". Block level
  below is free to breathe because those are block elements; this run is inline and is not.
-->
<!-- prettier-ignore -->
{#snippet spans(list: Span[])}{#each list as span, i (i)}{#if span.kind === 'text'}{span.text}{:else if span.kind === 'code'}{@const ref = codeLink?.resolve(span.text)}{#if ref}<button type="button" class="c-markdown__code c-markdown__path" title="Open in the Code tab" onclick={() => codeLink?.open(ref)}>{span.text}</button>{:else}<code class="c-markdown__code">{span.text}</code>{/if}{:else if span.kind === 'strong'}<strong>{@render spans(span.spans)}</strong>{:else if span.kind === 'em'}<em>{@render spans(span.spans)}</em>{:else if span.kind === 'strike'}<s>{@render spans(span.spans)}</s>{:else if span.kind === 'link'}<a href={span.href} onclick={(event) => open(event, span.href)}>{@render spans(span.spans)}</a>{:else if span.kind === 'file'}{@const ref = codeLink?.resolve(span.target)}{@const name = pathLabel(span.spans)}{#if ref && name !== null}<button type="button" class="c-markdown__code c-markdown__path" title="Open {span.target} in the Code tab" onclick={() => codeLink?.open(ref)}>{name}</button>{:else if ref}<button type="button" class="c-markdown__path" title="Open {span.target} in the Code tab" onclick={() => codeLink?.open(ref)}>{@render spans(span.spans)}</button>{:else if name !== null}<code class="c-markdown__code" title={span.target}>{name}</code>{:else}{@render spans(span.spans)}{/if}{/if}{/each}{/snippet}

<!--
  `depth` is how many lists deep a block sits. It exists for one thing, the marker on a nested
  bulleted list, which a selector could only reach as `.c-markdown ul ul`, one compound part past the
  stylesheet's limit.
-->
{#snippet flow(list: Block[], depth: number)}
  {#each list as block, i (i)}
    {#if block.kind === 'paragraph'}
      <p>{@render spans(block.spans)}</p>
    {:else if block.kind === 'heading'}
      <!-- A heading in a reply is a section of an answer, not a section of the app, so the level is
           carried through for structure and the stylesheet sizes them all far below the app's own. -->
      <svelte:element this={`h${Math.min(block.level + 2, 6)}`} class="c-markdown__heading">
        {@render spans(block.spans)}
      </svelte:element>
    {:else if block.kind === 'code'}
      {@const shell = shellSnippet(block.lang, block.raw)}
      <!-- No highlighting. The language is shown instead, because knowing a block is `rust` is most
           of what highlighting communicates here and the rest costs a dependency and a theme. -->
      <!-- The language and the actions sit in a bar above the code rather than floating over it.
           Floating, they needed a tall empty band at the top of every block to stay off the first
           line, which made a one-line command three lines tall. -->
      <div class="c-markdown__block">
        <div class="c-markdown__bar">
          <span class="c-markdown__lang">{block.lang ?? ''}</span>
          <span class="c-markdown__actions">
            <Button variant="quiet" size="sm" onclick={() => void copyBlock(block.raw)}>
              {copiedBlock === block.raw ? 'Copied' : 'Copy'}
            </Button>
            {#if onrunshell && shell}
              <Button
                variant="quiet"
                size="sm"
                disabled={!block.complete}
                title={block.complete
                  ? 'Review command before running'
                  : 'Waiting for the closing code fence'}
                onclick={() => onrunshell?.(shell)}>Run</Button
              >
            {:else if runnable(block.lang)}
              <Button
                variant="quiet"
                size="sm"
                title="Run read-only in the Database console"
                onclick={() => onrunsql?.(block.text)}>Run</Button
              >
            {/if}
          </span>
        </div>
        <pre>{block.text}</pre>
      </div>
    {:else if block.kind === 'list'}
      <!-- An item's child blocks — a nested list, a fenced example, a second paragraph — share one
           wrapper, so they line up with the item's text and are spaced from it and from each other
           the same way everywhere. -->
      {#if block.ordered}
        <ol class="c-markdown__ol" start={block.start}>
          {#each block.items as item, n (n)}
            <li>
              {@render spans(item.spans)}{#if item.children.length > 0}<div
                  class="c-markdown__item-blocks"
                >
                  {@render flow(item.children, depth + 1)}
                </div>{/if}
            </li>
          {/each}
        </ol>
      {:else}
        <ul class:c-markdown__list--nested={depth > 0}>
          {#each block.items as item, n (n)}
            <li>
              {@render spans(item.spans)}{#if item.children.length > 0}<div
                  class="c-markdown__item-blocks"
                >
                  {@render flow(item.children, depth + 1)}
                </div>{/if}
            </li>
          {/each}
        </ul>
      {/if}
    {:else if block.kind === 'table'}
      <!-- The wrapper owns overflow so a wide model-authored table scrolls inside its message
           rather than widening the whole pane. The table stays semantic for screen-reader navigation. -->
      <div class="c-markdown__table">
        <table>
          <thead>
            <tr>
              {#each block.header as cell, n (n)}
                <th
                  scope="col"
                  class:c-markdown__cell--center={block.alignments[n] === 'center'}
                  class:c-markdown__cell--right={block.alignments[n] === 'right'}
                >
                  {@render spans(cell)}
                </th>
              {/each}
            </tr>
          </thead>
          <tbody>
            {#each block.rows as row, n (n)}
              <tr>
                {#each row as cell, column (column)}
                  <td
                    class:c-markdown__cell--center={block.alignments[column] === 'center'}
                    class:c-markdown__cell--right={block.alignments[column] === 'right'}
                  >
                    {@render spans(cell)}
                  </td>
                {/each}
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    {:else if block.kind === 'quote'}
      <blockquote>{@render flow(block.blocks, depth)}</blockquote>
    {:else}
      <hr />
    {/if}
  {/each}
{/snippet}

<div class="c-markdown">{@render flow(blocks, 0)}</div>

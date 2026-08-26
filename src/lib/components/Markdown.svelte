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
  import { parse, type Block, type Span } from '../markdown';
  import Button from './ui/Button.svelte';

  const { source }: { source: string } = $props();

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

  let copiedBlock = $state<number | null>(null);
  let copyTimer: ReturnType<typeof setTimeout> | undefined;

  async function copyBlock(text: string, index: number) {
    try {
      await navigator.clipboard.writeText(text);
      copiedBlock = index;
      clearTimeout(copyTimer);
      copyTimer = setTimeout(() => (copiedBlock = null), 1400);
    } catch {
      /* Clipboard access can be denied. */
    }
  }
</script>

<!--
  Kept on one line, and that is not a formatting accident.

  Svelte turns the whitespace between template tags into text nodes, so breaking this across lines
  puts a space on either side of every span — `**no**where` would render as "no where". Block level
  below is free to breathe because those are block elements; this run is inline and is not.
-->
<!-- prettier-ignore -->
{#snippet spans(list: Span[])}{#each list as span, i (i)}{#if span.kind === 'text'}{span.text}{:else if span.kind === 'code'}<code class="c-markdown__code">{span.text}</code>{:else if span.kind === 'strong'}<strong>{@render spans(span.spans)}</strong>{:else if span.kind === 'em'}<em>{@render spans(span.spans)}</em>{:else if span.kind === 'strike'}<s>{@render spans(span.spans)}</s>{:else if span.kind === 'link'}<a href={span.href} onclick={(event) => open(event, span.href)}>{@render spans(span.spans)}</a>{/if}{/each}{/snippet}

{#snippet flow(list: Block[])}
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
      <!-- No highlighting. The language is shown instead, because knowing a block is `rust` is most
           of what highlighting communicates here and the rest costs a dependency and a theme. -->
      <div class="c-markdown__block">
        {#if block.lang}<span class="c-markdown__lang">{block.lang}</span>{/if}
        <span class="c-markdown__copy">
          <Button variant="quiet" size="sm" onclick={() => void copyBlock(block.text, i)}>
            {copiedBlock === i ? 'Copied' : 'Copy'}
          </Button>
        </span>
        <pre>{block.text}</pre>
      </div>
    {:else if block.kind === 'list'}
      {#if block.ordered}
        <ol start={block.start}>
          {#each block.items as item, n (n)}
            <li>{@render spans(item.spans)}{@render flow(item.children)}</li>
          {/each}
        </ol>
      {:else}
        <ul>
          {#each block.items as item, n (n)}
            <li>{@render spans(item.spans)}{@render flow(item.children)}</li>
          {/each}
        </ul>
      {/if}
    {:else if block.kind === 'quote'}
      <blockquote>{@render flow(block.blocks)}</blockquote>
    {:else}
      <hr />
    {/if}
  {/each}
{/snippet}

<div class="c-markdown">{@render flow(blocks)}</div>

<script lang="ts">
  /**
   * A message with its backticked names drawn as code: `ui` failed, rather than \`ui\` failed.
   *
   * The engine quotes role, skill and command names with backticks, because the same sentences go
   * to models, which read them as code. On screen the backticks are noise, and stripping them would
   * lose which words are names.
   */
  import { codeSpans } from '../review-run';

  const { text }: { text: string } = $props();
</script>

{#each codeSpans(text) as part, index (index)}{#if part.code}<code>{part.text}</code
    >{:else}{part.text}{/if}{/each}

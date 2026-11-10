<script lang="ts">
  /**
   * The public contract, searchable: every surface a plugin can add to, every event it can hear,
   * and every operation with whether a plugin may call it.
   *
   * `internal` operations are left out. They are window and process plumbing, not operations at
   * all (`Exposure::Internal` in `wtm-plugins`), and listing them told an author about calls no
   * declaration can name. Protected ones stay, because an author planning a plugin needs to know
   * that the thing they want is deliberately out of reach rather than missing.
   */
  import { catalog } from '../extensions';
  import TextInput from './ui/TextInput.svelte';

  const { id }: { id: string } = $props();

  let query = $state('');

  const KIND_WORDS: Record<string, string> = {
    style: 'styles',
    content: 'content',
    actions: 'actions',
    settings: 'settings',
  };
  const SCOPE_WORDS: Record<string, string> = {
    none: 'Public',
    context: 'Public · only in the worktree it runs in',
    arguments: 'Public · its arguments must be in scope',
    global: 'Public · refused to plugins limited to some worktrees',
  };

  const entries = [
    ...catalog.surfaces.map((s) => ({
      id: s.id,
      name: s.title,
      description: s.reason,
      status: s.kinds.length
        ? `Takes ${s.kinds.map((k) => KIND_WORDS[k] ?? k).join(', ')}`
        : 'Not open to plugins',
      version: s.version,
    })),
    ...catalog.events.map((e) => ({
      id: e.id,
      name: null,
      description: e.reason,
      status: 'Event',
      version: e.version,
    })),
    ...catalog.operations
      .filter((o) => o.exposure !== 'internal')
      .map((o) => ({
        id: o.id,
        name: null,
        description: o.reason,
        status:
          o.exposure === 'public'
            ? (SCOPE_WORDS[o.scope ?? 'none'] ?? 'Public')
            : 'Protected · no plugin can call it',
        version: o.version,
      })),
  ];
  const shown = $derived.by(() => {
    const q = query.trim().toLowerCase();
    return q
      ? entries.filter((c) =>
          `${c.id} ${c.name ?? ''} ${c.description} ${c.status}`.toLowerCase().includes(q),
        )
      : entries;
  });
</script>

<div class="c-extension-manager" {id}>
  <p class="c-note">
    API {catalog.api}: what wtm and plugins share. wtm still enforces every protected entry,
    and a provider can report that it does not support an operation.
  </p>
  <TextInput
    type="search"
    bind:value={query}
    ariaLabel="Search public contracts"
    placeholder="Chat, worktree, panel, settings…"
  />
  <p class="c-note" role="status">{shown.length} of {entries.length} entries</p>
  {#each shown as c (c.id)}<div class="c-extension-manager__row">
      <div>
        <code>{c.id}</code>{#if c.name}{` · ${c.name}`}{/if}
        <p class="c-note">{c.description}</p>
      </div>
      <span class="c-note">v{c.version} · {c.status}</span>
    </div>{/each}
</div>

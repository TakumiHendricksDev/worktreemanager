<script lang="ts">
  /**
   * A session's transcript, folded out of its flat event list.
   *
   * # Why the folding happens here and not in the store
   *
   * Both CLIs stream text a token at a time, so an assistant "message" is not something that
   * arrives — it is a run of consecutive `message_delta` events. Coalescing at render time keeps
   * the store a dumb append-only log, which means the rule for what counts as one message can
   * change without migrating anything, and an unrecognised `raw` event costs exactly one row
   * rather than needing a place in a message tree.
   *
   * # Why rows carry an explicit key
   *
   * A keyed `{#each}` needs a stable identity or it re-creates DOM on every append, which with a
   * streaming transcript is every few milliseconds. The array index is not stable — a bounded log
   * drops from the front — so a row's key is its kind plus the id or ordinal it was built from.
   */
  import type { AgendaStep, AgentEvent, AgentUsage } from '../ipc/types';
  import Markdown from './Markdown.svelte';

  const { events }: { events: AgentEvent[] } = $props();

  /**
   * One thing to draw.
   *
   * A union rather than a bag of optional fields, for the same reason `ApprovalRequest` is one in
   * Rust: a command card and a paragraph of prose have almost nothing in common, and a shared
   * shape would be mostly nulls.
   */
  type Row =
    | { key: string; kind: 'user'; text: string }
    | { key: string; kind: 'assistant'; text: string }
    | { key: string; kind: 'thinking'; text: string }
    | { key: string; kind: 'command'; command: string; output: string; exit: number | null }
    | {
        key: string;
        kind: 'tool';
        name: string;
        title: string | null;
        done: boolean;
        /** Null while running. The provider says whether it worked; the row has to show it. */
        ok: boolean | null;
        output: string | null;
      }
    | { key: string; kind: 'patch'; diff: string }
    | { key: string; kind: 'agenda'; explanation: string | null; steps: AgendaStep[] }
    | { key: string; kind: 'notice'; level: 'info' | 'warn' | 'error'; text: string }
    | { key: string; kind: 'usage'; usage: AgentUsage; costUsd: number | null }
    | { key: string; kind: 'raw'; provider: string; event: string; payload: unknown };

  const rows = $derived.by(() => {
    const out: Row[] = [];
    // Commands are addressed by id across three event kinds, so they need a lookup rather than
    // "the last row" — a second command can start before the first has finished.
    const commands = new Map<string, Extract<Row, { kind: 'command' }>>();
    const tools = new Map<string, Extract<Row, { kind: 'tool' }>>();
    let agenda: Extract<Row, { kind: 'agenda' }> | null = null;

    /** The assistant or thinking row a delta should append to, or null when a new one is needed. */
    function openAssistant(): Extract<Row, { kind: 'assistant' }> | null {
      const last = out.at(-1);
      return last !== undefined && last.kind === 'assistant' ? last : null;
    }
    function openThinking(): Extract<Row, { kind: 'thinking' }> | null {
      const last = out.at(-1);
      return last !== undefined && last.kind === 'thinking' ? last : null;
    }

    events.forEach((event, index) => {
      switch (event.kind) {
        case 'user_echo':
          out.push({ key: `u${index}`, kind: 'user', text: event.text });
          break;

        case 'message_delta': {
          const open = openAssistant();
          if (open) open.text += event.text;
          else out.push({ key: `a${index}`, kind: 'assistant', text: event.text });
          break;
        }

        case 'message':
          // A whole message from a provider that does not stream. When deltas already built a run
          // this is the same text arriving twice — the Codex adapter drops the duplicate at
          // source, and appending only when there is no open run is what keeps a
          // whole-message-only provider working without special-casing it here.
          if (!openAssistant()) {
            out.push({ key: `a${index}`, kind: 'assistant', text: event.text });
          }
          break;

        case 'reasoning_delta': {
          const open = openThinking();
          if (open) open.text += event.text;
          else out.push({ key: `t${index}`, kind: 'thinking', text: event.text });
          break;
        }

        case 'command_started': {
          const row: Extract<Row, { kind: 'command' }> = {
            key: `c${event.id}`,
            kind: 'command',
            command: event.command,
            output: '',
            exit: null,
          };
          commands.set(event.id, row);
          out.push(row);
          break;
        }

        case 'command_output': {
          const row = commands.get(event.id);
          if (row) row.output += event.chunk;
          break;
        }

        case 'command_finished': {
          const row = commands.get(event.id);
          if (row) row.exit = event.exitCode;
          break;
        }

        case 'tool_started': {
          const row: Extract<Row, { kind: 'tool' }> = {
            key: `l${event.id}`,
            kind: 'tool',
            name: event.name,
            title: event.title,
            done: false,
            ok: null,
            output: null,
          };
          tools.set(event.id, row);
          out.push(row);
          break;
        }

        case 'tool_finished': {
          // `ok` and `output` were arriving and being dropped, so a tool that failed rendered
          // exactly like one that worked — the transcript said a thing had been attempted and never
          // whether it succeeded.
          const row = tools.get(event.id);
          if (row) {
            row.done = true;
            row.ok = event.ok;
            row.output = event.output;
          }
          break;
        }

        case 'patch':
          out.push({ key: `p${index}`, kind: 'patch', diff: event.unifiedDiff });
          break;

        case 'agenda_updated':
          // Replaced rather than appended: the provider sends the whole list on every change, so
          // a row per update would be the same plan a dozen times with one checkbox different.
          if (agenda) {
            agenda.explanation = event.explanation;
            agenda.steps = event.steps;
          } else {
            agenda = {
              key: 'agenda',
              kind: 'agenda',
              explanation: event.explanation,
              steps: event.steps,
            };
            out.push(agenda);
          }
          break;

        case 'notice':
          out.push({
            key: `n${index}`,
            kind: 'notice',
            level: event.level,
            text: event.message,
          });
          break;

        case 'failed':
          out.push({
            key: `f${index}`,
            kind: 'notice',
            level: 'error',
            text: event.message,
          });
          break;

        case 'turn_finished':
          out.push({
            key: `g${index}`,
            kind: 'usage',
            usage: event.usage,
            costUsd: event.costUsd,
          });
          break;

        case 'raw':
          // Never dropped. Both protocols are experimental and grow event kinds in patch releases,
          // so a collapsed row is what makes a CLI upgrade noisier rather than broken.
          out.push({
            key: `r${index}`,
            kind: 'raw',
            provider: event.provider,
            event: event.event,
            payload: event.payload,
          });
          break;

        // Deliberately not drawn. `session_ready` and `turn_started` are state the pane header
        // shows; a mid-turn `usage` is superseded by the one on `turn_finished`; approvals get
        // their own card in the increment that can answer them, and `approval_resolved` only ever
        // removes one. Listed rather than defaulted, so a new event kind is a type error here.
        case 'session_ready':
        case 'turn_started':
        case 'usage':
        case 'approval_requested':
        case 'approval_resolved':
          break;
      }
    });

    return out;
  });

  /**
   * The state class for a step.
   *
   * A function returning a union rather than interpolating `is-{step.status}` into the markup, for
   * two reasons. The domain spells the middle state `in_progress`, and BEMIT state classes in this
   * app are kebab — `.is-selected`, `.is-dragging` — so the raw value would introduce the only
   * underscored class in the stylesheet. And with the stylesheet global there is nothing that
   * catches a class name that does not exist; a typed return is the one mechanism that does, which
   * is why every UI primitive here states its contract that way.
   */
  function stepClass(
    status: AgendaStep['status'],
  ): 'is-pending' | 'is-in-progress' | 'is-done' {
    if (status === 'completed') return 'is-done';
    if (status === 'in_progress') return 'is-in-progress';
    return 'is-pending';
  }

  /** The status as a word, for the label beside the class. */
  function stepLabel(status: AgendaStep['status']): string {
    return status === 'in_progress' ? 'in progress' : status;
  }

  function tokens(usage: AgentUsage): string {
    const parts = [
      `${usage.tokensIn.toLocaleString()} in`,
      `${usage.tokensOut.toLocaleString()} out`,
    ];
    if (usage.cached > 0) parts.push(`${usage.cached.toLocaleString()} cached`);
    return parts.join(' · ');
  }

  /**
   * What a turn cost, when the provider says.
   *
   * Carried on the event since it was added and never drawn. Four decimal places because a turn is
   * routinely under a cent and `$0.00` for everything tells nobody anything.
   */
  function cost(usd: number | null): string | null {
    if (usd === null || usd <= 0) return null;
    return `$${usd < 0.01 ? usd.toFixed(4) : usd.toFixed(2)}`;
  }

  /**
   * Split a unified diff into classified lines.
   *
   * The gutter character stays in the text and is not replaced by the colour: `_semantic.scss` is
   * explicit that nothing in this app encodes state in colour alone, and a diff read by someone who
   * cannot separate red from green is exactly the case that rule exists for.
   */
  function diffLines(
    diff: string,
  ): { text: string; cls: 'is-add' | 'is-del' | 'is-hunk' | 'is-meta' | '' }[] {
    return diff.split('\n').map((text) => {
      if (text.startsWith('+++') || text.startsWith('---'))
        return { text, cls: 'is-meta' } as const;
      if (text.startsWith('@@')) return { text, cls: 'is-hunk' } as const;
      if (text.startsWith('+')) return { text, cls: 'is-add' } as const;
      if (text.startsWith('-')) return { text, cls: 'is-del' } as const;
      return { text, cls: '' } as const;
    });
  }
</script>

<div class="c-transcript">
  {#each rows as row (row.key)}
    {#if row.kind === 'user'}
      <p class="c-transcript__user">{row.text}</p>
    {:else if row.kind === 'assistant'}
      <!-- The one place arbitrary document structure appears. Rendered as elements rather than a
           string of HTML, so nothing a model writes can become markup — see `markdown.ts`. -->
      <div class="c-transcript__said"><Markdown source={row.text} /></div>
    {:else if row.kind === 'thinking'}
      <!-- Collapsed by default: thinking is useful when you want it and noise when you do not.
           A `<details>` rather than a state class, because the browser owns the disclosure. -->
      <details class="c-transcript__thinking">
        <summary>Thinking</summary>
        <p>{row.text}</p>
      </details>
    {:else if row.kind === 'command'}
      <div class="c-transcript__card">
        <code class="c-transcript__cmd">{row.command}</code>
        {#if row.output}
          <pre class="c-transcript__out">{row.output}</pre>
        {/if}
        {#if row.exit !== null}
          <span class={row.exit === 0 ? 'c-status--ok' : 'c-status--danger'}
            >exit {row.exit}</span
          >
        {/if}
      </div>
    {:else if row.kind === 'tool'}
      <!--
        The name *is* the disclosure, so a tool call is one line.

        It used to be two — a name, then a separate `▸ output` beneath it — and a turn that ran
        seven tools was fourteen rows of machinery with paragraph-sized gaps between them, which
        buried the actual conversation. There is nothing on the second row worth its own line: the
        name says what ran and the status says how it went.

        Never opened unprompted, including on failure. Auto-opening a failure was tried and is
        wrong here: agents run speculative commands constantly — checking whether a file exists,
        a `git` call on a branch with no upstream — so "failed" is routine and an expanded block
        per occurrence is exactly the sprawl this row is trying to avoid. The word `failed` is what
        tells you to click.
      -->
      {#if row.output}
        <details class="c-transcript__tool">
          <summary class="c-transcript__tool-name">
            {row.title ?? row.name}
            {#if row.ok === false}<span class="c-status--danger">failed</span>{/if}
          </summary>
          <pre class="c-transcript__out">{row.output}</pre>
        </details>
      {:else}
        <!-- Nothing to disclose. A `<details>` with an empty body offers a marker that does
             nothing, which is worse than no marker. -->
        <p class="c-transcript__tool-name c-transcript__tool-name--bare">
          {row.title ?? row.name}
          {#if !row.done}
            <span class="c-status--subtle">running</span>
          {:else if row.ok === false}
            <span class="c-status--danger">failed</span>
          {/if}
        </p>
      {/if}
    {:else if row.kind === 'patch'}
      <pre class="c-transcript__diff">{#each diffLines(row.diff) as line, i (i)}<span
            class="c-transcript__diff-line {line.cls}"
            >{line.text}
</span>{/each}</pre>
    {:else if row.kind === 'agenda'}
      <div class="c-transcript__card">
        {#if row.explanation}<p>{row.explanation}</p>{/if}
        <ol class="c-transcript__steps">
          {#each row.steps as step, i (i)}
            <!-- The status is a word as well as a class: nothing in this app encodes state in
                 colour alone, and a checked-off step is exactly where that rule bites. -->
            <li class="c-transcript__step {stepClass(step.status)}">
              {step.text}
              <span class="c-transcript__step-state">{stepLabel(step.status)}</span>
            </li>
          {/each}
        </ol>
      </div>
    {:else if row.kind === 'notice'}
      <p
        class={row.level === 'error'
          ? 'c-transcript__note c-status--danger'
          : 'c-transcript__note c-status--warn'}
      >
        {row.text}
      </p>
    {:else if row.kind === 'usage'}
      <p class="c-transcript__usage">
        {tokens(row.usage)}{#if cost(row.costUsd)}&nbsp;· {cost(row.costUsd)}{/if}
      </p>
    {:else}
      <!-- An event this build does not know. Shown, because dropping it would lose information
           with no trace, and collapsed, because it is usually not interesting. -->
      <details class="c-transcript__raw">
        <summary>{row.provider} · {row.event}</summary>
        <pre>{JSON.stringify(row.payload, null, 2)}</pre>
      </details>
    {/if}
  {/each}
</div>

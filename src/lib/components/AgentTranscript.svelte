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
    | { key: string; kind: 'tool'; name: string; title: string | null; done: boolean }
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
          };
          tools.set(event.id, row);
          out.push(row);
          break;
        }

        case 'tool_finished': {
          const row = tools.get(event.id);
          if (row) row.done = true;
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
</script>

<div class="c-transcript">
  {#each rows as row (row.key)}
    {#if row.kind === 'user'}
      <p class="c-transcript__user">{row.text}</p>
    {:else if row.kind === 'assistant'}
      <p class="c-transcript__said">{row.text}</p>
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
      <p class="c-transcript__tool">
        {row.title ?? row.name}
        {#if !row.done}<span class="c-status--subtle">running</span>{/if}
      </p>
    {:else if row.kind === 'patch'}
      <pre class="c-transcript__diff">{row.diff}</pre>
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
      <p class="c-transcript__usage">{tokens(row.usage)}</p>
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

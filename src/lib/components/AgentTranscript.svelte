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
  import type { AgendaStep, AgentAttachment, AgentEvent, AgentUsage } from '../ipc/types';
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
    | { key: string; kind: 'attachments'; attachments: AgentAttachment[] }
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
    | { key: string; kind: 'raw'; provider: string; event: string; payload: unknown }
    | {
        key: string;
        kind: 'steps';
        steps: Step[];
        /** Members that failed, so a collapsed group cannot hide one. */
        failed: number;
        /** True while the turn is still working. See `group()`. */
        live: boolean;
      };

  /** A row that reports work rather than saying anything: the members of a `steps` group. */
  type Step = Extract<Row, { kind: 'tool' | 'command' | 'raw' }>;

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
        case 'attachments':
          out.push({
            key: `h${index}`,
            kind: 'attachments',
            attachments: event.attachments,
          });
          break;

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
        // removes one; `skills_listed` is the composer's `/` menu, not a thing that happened.
        case 'session_ready':
        case 'turn_started':
        case 'usage':
        case 'approval_requested':
        case 'approval_resolved':
        case 'skills_listed':
          break;

        /*
         * The exhaustiveness check, which the comment above used to only *claim*.
         *
         * A `switch` over a discriminated union proves nothing on its own inside a `forEach` —
         * there is no return type to be incomplete — so `skills_listed` was added to the union and
         * silently fell through here without `svelte-check` saying a word. It happened to want
         * ignoring, which is the bad kind of luck: the next kind might have been a message.
         *
         * Assigning the narrowed `event` to `never` is what makes an unhandled kind a build error.
         * A new event must now be listed above, even if the decision is to draw nothing.
         */
        default: {
          const unreachable: never = event;
          void unreachable;
        }
      }
    });

    return group(out);
  });

  /**
   * How many machinery rows it takes before they are worth folding away.
   *
   * A group of one is strictly worse than the row it replaces — same height, less information, one
   * more click. Two is a wash. Three is where a run starts to read as a wall.
   */
  const MIN_GROUP = 3;

  /**
   * How long a run of machinery gets before it is broken up regardless.
   *
   * Reasoning is what normally paces a turn — a model narrates, works, narrates again — so most
   * groups end long before this. A model that is *not* thinking out loud still produces long runs,
   * though, and one line reading "40 steps" is a wall with a marker on it: nothing about it says
   * which part of the turn you are looking at, and the only way to find out is to open all forty.
   * Breaking at a bound gives the run a shape without needing anything to interrupt it.
   */
  const MAX_GROUP = 12;

  /**
   * Fold runs of machinery into one row each.
   *
   * A second pass over the finished list rather than a change to the fold above, and deliberately
   * so: that loop correlates commands and tools by id through `Map`s and mutates rows in place, and
   * this needs none of that. Keeping them separate means grouping cannot break correlation.
   *
   * What counts as machinery is `tool`, `command` and `raw` — rows that report *that* work happened.
   * A patch, an agenda, a notice and a usage line are content, so they end a run: the answer and the
   * things the answer is made of stay at full weight, which is the entire point.
   *
   * # Reasoning is content, and that is the change that makes a turn readable
   *
   * It used to be folded *into* the group, on the argument that fifteen rows each reading `Thinking`
   * and holding nothing visible said less than one row holding all fifteen. The premise was right and
   * the conclusion was wrong: the fix for a row that says nothing is to make it say something, not to
   * hide it. Absorbing it also meant nothing ever interrupted a run, so a turn that ran forty tools
   * across six separate trains of thought collapsed into **one** `<details>` — and the narration that
   * explained the forty was buried two disclosures deep inside it.
   *
   * Treating it as content instead gives the turn the shape it actually has: a sentence about what
   * the model is about to do, the handful of steps it took, another sentence, more steps. Which is
   * what the reader wanted from the transcript in the first place, and it costs one line moved.
   */
  function group(rows: Row[]): Row[] {
    const out: Row[] = [];
    let pending: Step[] = [];

    /** Emit whatever has accumulated: a group if it earned one, the bare rows if it did not. */
    function flush(live: boolean) {
      if (pending.length === 0) return;

      if (pending.length >= MIN_GROUP) {
        out.push({
          // Keyed off the first member, never the size: a group that grows from three to four
          // mid-stream has to keep its key or Svelte remounts the whole subtree on every tool call,
          // which is the churn the header comment on keys exists to prevent.
          key: `s:${pending[0]?.key}`,
          kind: 'steps',
          steps: pending,
          failed: pending.filter(isFailed).length,
          live,
        });
      } else {
        out.push(...pending);
      }
      pending = [];
    }

    for (const row of rows) {
      if (isStep(row)) {
        pending.push(row);
        // Bounded, so no single group can swallow a whole turn. Not live: the run did not end
        // because the turn moved on, but there is more machinery coming either way, and the *last*
        // group of a trailing run is the one that gets the marker.
        if (pending.length >= MAX_GROUP) flush(false);
        continue;
      }
      // Content — including reasoning. Whatever was accumulating ends here, and it ended because
      // the turn moved on, so it is never the live one whatever follows it.
      flush(false);
      out.push(row);
    }

    /*
     * A trailing run is a turn still working.
     *
     * No turn tracking needed for this: `turn_finished` pushes a `usage` row, so a turn that has
     * ended never leaves machinery last. Cheaper than reading turn ids, and more robust — Codex's
     * comes from `.unwrap_or_default()` and can be the empty string.
     */
    flush(true);
    return out;
  }

  /** How much of a command fits on a summary line before it stops being a line. */
  const COMMAND_CHARS = 96;

  /**
   * A shell wrapper and the quoting it brings, so a command can be read at a glance.
   *
   * Codex sends the argv it actually ran, which for every shell step is
   * `/bin/zsh -lc "rg -n \"class ExcelExport\" apps/exports && sed -n '820,890p' …"`. Rendered
   * whole that is five to eight wrapped lines of which the first twelve characters are the same
   * every time, and six of them filled the pane. What the reader wants is the command; `/bin/zsh
   * -lc` is how it was delivered.
   *
   * Only the wrapper is stripped, and only when it is unambiguously there — a command that arrives
   * bare (Claude's tools, or a provider that does not wrap) has to pass through untouched, so this
   * matches an explicit list of shells rather than guessing at the first quoted argument.
   */
  function shortCommand(command: string): string {
    const wrapped =
      /^\s*(?:\/(?:usr\/)?bin\/)?(?:ba|z|da)?sh\s+-[a-z]*c\s+(['"])([\s\S]*)\1\s*$/.exec(
        command,
      );
    // Group 2 is the whole quoted body. Inner escapes of the *same* quote survive as written —
    // unescaping them would be a shell parser, and this is a label.
    const body = (wrapped?.[2] ?? command).replace(/\\(["'])/g, '$1');
    const line = body.replace(/\s+/g, ' ').trim();
    return line.length > COMMAND_CHARS ? `${line.slice(0, COMMAND_CHARS - 1)}…` : line;
  }

  /** How much reasoning fits on a summary line before it stops being a line. */
  const THINKING_CHARS = 120;

  /**
   * The opening of a reasoning run, as one line.
   *
   * The `<summary>` used to be the literal word `Thinking`, which is the same failure the tool rows
   * had before a06b05a: a disclosure whose label describes its *category* rather than its contents
   * makes the reader open it to find out whether they wanted to. Both CLIs put a short statement of
   * intent at the front of a reasoning block — "I need to check how the config is loaded first" — so
   * the first sentence is very close to the narration line the transcript wants, and it is free.
   *
   * The whole text is still one click away, and unlike a tool's output it is *always* worth having:
   * the summary is a prefix of the body rather than a name for it.
   *
   * Cut at a sentence boundary when there is one near the front, because a clause ending mid-word
   * reads as damage. Falls back to a hard truncation, then to the category word for a block that
   * somehow arrived empty — a `<summary>` with nothing in it is an invisible control.
   *
   * # Why a leading `**heading**` is a case of its own
   *
   * Because Codex writes them, routinely: its reasoning summaries arrive as `**Checking the loader**`
   * followed by the prose. This is a `<summary>`, not `<Markdown>` — deliberately, since nothing a
   * model writes may become markup here — so the asterisks would be on screen as themselves. A
   * model-written heading is also a *better* summary than the first sentence of the prose under it,
   * which is why this looks for one rather than merely tolerating it.
   *
   * Only that one construct, and only at the front. Anything more is a markdown parser, and there is
   * already one of those in `markdown.ts` for the place that needs it.
   */
  function firstLine(text: string): string {
    const line = text.replace(/\s+/g, ' ').trim();
    if (line === '') return 'Thinking';

    const heading = /^\*\*\s*(.+?)\s*\*\*/.exec(line);
    if (heading?.[1]) return clip(heading[1]);

    const stop = /[.!?](?:\s|$)/.exec(line);
    if (stop && stop.index < THINKING_CHARS) return clip(line.slice(0, stop.index + 1));
    return clip(line);
  }

  function clip(text: string): string {
    return text.length > THINKING_CHARS ? `${text.slice(0, THINKING_CHARS - 1)}…` : text;
  }

  /** Whether a row reports work rather than saying something. */
  function isStep(row: Row): row is Step {
    return row.kind === 'tool' || row.kind === 'command' || row.kind === 'raw';
  }

  function isFailed(step: Step): boolean {
    if (step.kind === 'tool') return step.ok === false;
    if (step.kind === 'command') return step.exit !== null && step.exit !== 0;
    return false;
  }

  /**
   * What a step is called, in a group's summary and in its list.
   *
   * Not `stepLabel`: that name is taken by the agenda's status word, and the two are unrelated —
   * an agenda step is something the model plans to do, this is something it did.
   */
  function labelOf(step: Step): string {
    if (step.kind === 'tool') return step.title ?? step.name;
    if (step.kind === 'command') return shortCommand(step.command);
    return `${step.provider} · ${step.event}`;
  }

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

  function formatBytes(size: number): string {
    if (size < 1024) return `${size} B`;
    if (size < 1024 * 1024) return `${Math.round(size / 1024)} KB`;
    return `${(size / (1024 * 1024)).toFixed(1)} MB`;
  }
</script>

<div class="c-transcript">
  {#each rows as row (row.key)}
    {#if row.kind === 'user'}
      <p class="c-transcript__user">{row.text}</p>
    {:else if row.kind === 'attachments'}
      <div class="c-transcript__attachments" aria-label="User attachments">
        {#each row.attachments as attachment (attachment.path)}
          <figure class="c-transcript__attachment">
            {#if attachment.mime.startsWith('image/')}
              <img
                src="data:{attachment.mime};base64,{attachment.dataBase64}"
                alt="{attachment.name} attachment"
              />
            {:else}
              <span class="c-transcript__attachment-file"
                >{attachment.name.split('.').at(-1)}</span
              >
            {/if}
            <figcaption>
              <span>{attachment.name}</span>
              <small>{formatBytes(attachment.size)}</small>
            </figcaption>
          </figure>
        {/each}
      </div>
    {:else if row.kind === 'assistant'}
      <!-- The one place arbitrary document structure appears. Rendered as elements rather than a
           string of HTML, so nothing a model writes can become markup — see `markdown.ts`. -->
      <div class="c-transcript__said"><Markdown source={row.text} /></div>
    {:else if row.kind === 'thinking'}
      <!--
        The narration line, and the reason it is one line rather than the word `Thinking`.

        A run of reasoning is what the model was about to do and why, which is the one thing a reader
        scanning a turn actually wants — so its opening sentence is on screen rather than behind a
        marker that says only which *category* of thing is hidden. The rest is still one click away,
        and the summary is a prefix of the body rather than a name for it, so opening it never
        contradicts what was already read.

        Still a `<details>` rather than a state class, because the browser owns the disclosure — and
        still collapsed by default: a whole reasoning block is useful when you want it and a wall when
        you do not.
      -->
      <details class="c-transcript__thinking">
        <summary>{firstLine(row.text)}</summary>
        <p>{row.text}</p>
      </details>
    {:else if isStep(row)}
      {@render step(row)}
    {:else if row.kind === 'steps'}
      <!--
        A run of work, as one line.

        Collapsed always, including while it is being added to. A turn that ran forty tools showed
        forty rows and pushed the answer off the screen entirely; the count plus the step currently
        underway is the part anyone reads, and the rest is a click away. Nothing is dropped — the
        body below is exactly the list that used to be inline.

        A run now ends at the next thing the model *says*, reasoning included, so these are the gaps
        between narration rather than one box per turn. See `group()`.
      -->
      <details class="c-transcript__group">
        <summary class="c-transcript__group-name">
          <span class="c-transcript__group-count">
            {row.steps.length}
            {row.steps.length === 1 ? 'step' : 'steps'}
          </span>
          {#if row.failed > 0}
            <!-- Surfaced on the closed row, because a collapsed group must not be able to hide a
                 failure — that would make folding a way to lose information rather than defer it. -->
            <span class="c-status--danger">{row.failed} failed</span>
          {/if}
          {#if row.live}
            <span class="c-transcript__group-now"
              >{labelOf(row.steps[row.steps.length - 1]!)}</span
            >
          {/if}
        </summary>
        <div class="c-transcript__group-body">
          {#each row.steps as member (member.key)}
            {@render step(member)}
          {/each}
        </div>
      </details>
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
    {/if}
  {/each}
</div>

<!--
  One machinery item, wherever it appears.

  Rendered from a snippet because a step shows up in two places — loose, when its run was too short
  to fold, and inside a group when it was not — and the two must be indistinguishable. Two copies of
  this markup is how they would stop being.

  Every kind is the same shape: one line that discloses. That uniformity is what lets a group hold a
  mix of tool calls, shell commands and unknown events without reading as three different things.
-->
{#snippet step(row: Step)}
  {#if row.kind === 'command'}
    <!--
      The command, not the argv that delivered it.

      Codex reports what it actually ran, which for every shell step begins `/bin/zsh -lc "…"` and
      wraps across five to eight lines in the card this used to be. Six of them filled the pane. The
      summary is the command with that wrapper stripped and truncated; the untouched original is the
      first thing inside, because a label you cannot verify is worse than no label.
    -->
    <details class="c-transcript__tool">
      <summary class="c-transcript__tool-name">
        <code class="c-transcript__cmd">{shortCommand(row.command)}</code>
        {#if row.exit !== null && row.exit !== 0}
          <span class="c-status--danger">exit {row.exit}</span>
        {/if}
      </summary>
      <pre class="c-transcript__out">{row.command}</pre>
      {#if row.output}<pre class="c-transcript__out">{row.output}</pre>{/if}
    </details>
  {:else if row.kind === 'raw'}
    <!-- An event this build does not know. Shown, because dropping it would lose information with
         no trace, and collapsed, because it is usually not interesting. -->
    <details class="c-transcript__raw">
      <summary>{row.provider} · {row.event}</summary>
      <pre>{JSON.stringify(row.payload, null, 2)}</pre>
    </details>
  {:else if row.output}
    <!--
      The name *is* the disclosure, so a tool call is one line.

      Never opened unprompted, including on failure: agents run speculative commands constantly —
      checking whether a file exists, a `git` call on a branch with no upstream — so "failed" is
      routine, and an expanded block per occurrence is the sprawl this row exists to avoid. The word
      `failed` is what tells you to click.
    -->
    <details class="c-transcript__tool">
      <summary class="c-transcript__tool-name">
        {row.title ?? row.name}
        {#if row.ok === false}<span class="c-status--danger">failed</span>{/if}
      </summary>
      <pre class="c-transcript__out">{row.output}</pre>
    </details>
  {:else}
    <!-- Nothing to disclose. A `<details>` with an empty body offers a marker that does nothing,
         which is worse than no marker. -->
    <p class="c-transcript__tool-name c-transcript__tool-name--bare">
      {row.title ?? row.name}
      {#if !row.done}
        <span class="c-status--subtle">running</span>
      {:else if row.ok === false}
        <span class="c-status--danger">failed</span>
      {/if}
    </p>
  {/if}
{/snippet}

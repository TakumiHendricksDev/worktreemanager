/**
 * Turning a dead session's transcript into the first turn of a live one.
 *
 * # Why the live pane and not the provider's own store
 *
 * Both CLIs keep a durable transcript, and `wtm-agent` already has private readers for them —
 * `claude_history_from` reads `~/.claude/projects/*.jsonl`, Codex's arrives on `thread/resume`. They
 * would be the higher-fidelity source, and they are the follow-up if this proves too lossy. They are
 * not the v1 for two reasons: exposing them means a new command carrying a full conversation across
 * the IPC boundary, and the pane already holds the same messages in memory, filtered to exactly the
 * rows a reader sees. `sessions.svelte.ts` bounds that log at 20 000 events, which for the message
 * rows this keeps is far more than the character budget below will accept anyway.
 *
 * Nothing is written to disk here. The digest exists only as the text of one outgoing turn, and the
 * receiving CLI stores it in its own transcript under its own permissions — which is the policy
 * `wtm-config`'s `sessions.rs` states and the reason wtm keeps no transcript of its own.
 *
 * # Why a plain module with no imports from state
 *
 * There is no JS test runner here, so logic that only runs inside a component cannot be checked at
 * all. Keeping the folding and the truncation as one pure function of an event array is what makes
 * it *readable* as a unit, and it is the same reason `suggest.ts` and `markdown.ts` are files rather
 * than component internals.
 */

import type { AgentEvent } from './ipc/types';

/**
 * How much transcript the seed prompt may carry.
 *
 * A budget in characters rather than tokens because there is no tokenizer on this side and a rough
 * bound is all this needs: roughly 6k tokens of history, which leaves an ordinary context window
 * overwhelmingly free for the work itself. The receiving session is *starting*, and a first turn
 * that fills half its window with somebody else's conversation would trade one exhausted session
 * for another.
 */
const MAX_TOTAL_CHARS = 24_000;

/**
 * How much of a single message survives.
 *
 * One 60k-character tool dump pasted into a prompt would otherwise consume the whole budget and
 * evict the entire conversation around it. The middle goes rather than the tail, because the last
 * lines of a long message are usually its conclusion.
 */
const MAX_MESSAGE_CHARS = 4_000;

const TRUNCATION_MARK = '\n\n[… middle of this message omitted …]\n\n';

interface Line {
  speaker: 'User' | 'Assistant';
  text: string;
}

function clamp(text: string): string {
  if (text.length <= MAX_MESSAGE_CHARS) return text;
  const half = Math.floor((MAX_MESSAGE_CHARS - TRUNCATION_MARK.length) / 2);
  return text.slice(0, half) + TRUNCATION_MARK + text.slice(-half);
}

/**
 * Fold the event log into alternating speaker lines.
 *
 * Streaming deltas are concatenated into the message they belong to, and a complete `message` event
 * replaces the deltas that preceded it — the same "the deltas already showed this" rule
 * `AgentTranscript` applies, expressed here as: a `message` closes the open assistant run whether or
 * not deltas built one.
 *
 * Everything that is not a user or assistant *message* is dropped: no reasoning, no tool calls, no
 * diffs. Not for size — for usefulness. A tool call is a thing that already happened to the
 * filesystem, and replaying it as context invites the new session to do it again. The instruction
 * text below tells it to read the current state instead, which is true regardless of what the
 * transcript said.
 */
function fold(events: AgentEvent[]): Line[] {
  const lines: Line[] = [];
  let streaming: string | null = null;

  const flush = () => {
    if (streaming !== null && streaming.trim()) {
      lines.push({ speaker: 'Assistant', text: streaming.trim() });
    }
    streaming = null;
  };

  for (const event of events) {
    if (event.kind === 'user_echo') {
      flush();
      if (event.text.trim()) lines.push({ speaker: 'User', text: event.text.trim() });
    } else if (event.kind === 'message_delta') {
      streaming = (streaming ?? '') + event.text;
    } else if (event.kind === 'message') {
      flush();
      if (event.text.trim()) lines.push({ speaker: 'Assistant', text: event.text.trim() });
    }
  }
  flush();
  return lines;
}

/**
 * The prompt that starts the continuing session.
 *
 * Keeps the **most recent** turns and drops from the front, because the end of a conversation is
 * where the work is. A dropped beginning is announced in the text rather than silently omitted: a
 * model told it is seeing a partial transcript asks about what it is missing, where one that assumes
 * it has everything invents the beginning.
 */
export function transferPrompt(events: AgentEvent[], fromLabel: string): string {
  const lines = fold(events).map((line) => ({ ...line, text: clamp(line.text) }));

  const kept: string[] = [];
  let budget = MAX_TOTAL_CHARS;
  let dropped = false;
  for (let i = lines.length - 1; i >= 0; i -= 1) {
    const line = lines[i];
    if (!line) continue;
    const rendered = `[${line.speaker}]: ${line.text}`;
    if (rendered.length > budget) {
      dropped = true;
      break;
    }
    budget -= rendered.length;
    kept.unshift(rendered);
  }

  const preamble = [
    `You are continuing a conversation that began with ${fromLabel} in this same worktree.`,
    'That session ran out of usage, so the work moves to you.',
    dropped
      ? 'Below is the end of the transcript so far; the earlier part has been cut.'
      : 'Below is the transcript so far.',
    'Pick up exactly where it stopped: do not start over, and do not redo work that is already',
    'done. Anything the transcript describes as finished may already be on disk, so read the',
    'current state of a file before you change it.',
  ].join(' ');

  const body =
    kept.length > 0
      ? `--- transcript ---\n\n${kept.join('\n\n')}\n\n--- end of transcript ---`
      : '(The previous session had no messages to carry over.)';

  return `${preamble}\n\n${body}\n\nContinue the task.`;
}

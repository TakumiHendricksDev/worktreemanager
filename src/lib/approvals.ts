/**
 * What an approval is asking, in a few words, for a list that shows many of them at once.
 *
 * The approval card is built to be read: a whole command, a diff, a plan. Home's Needs-you list
 * stacks every session's waiting approvals, and a row there has to say which one is which before
 * the card for it is opened — so it gets a kind and one line, and the card stays the place the
 * decision is made.
 *
 * Pure, like `status.ts`, so the wording can be checked without mounting anything.
 */

import type { ApprovalRequest } from './ipc/types';

export interface ApprovalSummary {
  /** One word for the kind of thing being asked. */
  kind: 'command' | 'change' | 'permissions' | 'plan' | 'tool' | 'question';
  /** One line of what it is. */
  line: string;
}

/** The kind words as a row shows them. */
export const APPROVAL_WORD: Record<ApprovalSummary['kind'], string> = {
  command: 'Run a command',
  change: 'Apply a change',
  permissions: 'Grant permissions',
  plan: 'Review a plan',
  tool: 'Use a tool',
  question: 'Answer a question',
};

function firstLine(text: string, max = 96): string {
  const line = text.trim().split('\n')[0] ?? '';
  return line.length > max ? `${line.slice(0, max)}…` : line;
}

/** The file a unified diff touches first, or null. */
function firstFile(diff: string): string | null {
  const match = /^\+\+\+ (?:b\/)?(.+)$/m.exec(diff) ?? /^diff --git a\/(\S+)/m.exec(diff);
  return match?.[1] ?? null;
}

export function summarize(request: ApprovalRequest): ApprovalSummary {
  switch (request.kind) {
    case 'command':
      return { kind: 'command', line: firstLine(request.command) };
    case 'file_change': {
      const file = firstFile(request.unified_diff);
      return { kind: 'change', line: file ?? firstLine(request.reason ?? 'A file change') };
    }
    case 'permissions':
      return { kind: 'permissions', line: firstLine(request.summary) };
    case 'plan_review':
      return {
        kind: 'plan',
        line: firstLine(request.markdown.replace(/^#+\s*/, '')) || 'A plan',
      };
    case 'tool_input':
      return { kind: 'tool', line: request.tool };
    case 'user_input': {
      const first = request.questions[0];
      const more =
        request.questions.length > 1 ? ` (+${request.questions.length - 1})` : '';
      return {
        kind: 'question',
        line: `${firstLine(first?.question ?? 'A question')}${more}`,
      };
    }
  }
}

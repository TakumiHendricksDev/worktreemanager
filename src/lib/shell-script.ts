/** The reviewed bytes and interpreter travel together; a fence is never guessed from its body. */
import type { ExitOutcome } from './ipc/types';

export type ShellInterpreter = 'sh' | 'bash' | 'zsh';
export interface ShellSnippet {
  command: string;
  interpreter: ShellInterpreter;
}
export interface ShellRunRequest extends ShellSnippet {
  key: string;
  projectId: string;
  worktreeId: string;
  shell: string;
}
export interface ShellRun {
  id: string;
  request: ShellRunRequest;
  requester: string | null;
  directory: string;
  phase:
    | 'prepared'
    | 'awaiting_approval'
    | 'awaiting_pane'
    | 'opening_shell'
    | 'reserved'
    | 'running'
    | 'completed'
    | 'denied'
    | 'cancelled'
    | 'interrupted';
  session: string | null;
  outcome: ExitOutcome | null;
  problem: string | null;
}
export interface HomeShellGrant {
  projectId: string;
  worktreeId: string;
  directory: string;
  valid: boolean;
}
export interface RunShell {
  session: string;
  project: string;
  worktree: string;
  availability: 'ready' | 'busy' | 'unknown' | 'reserved' | 'running' | 'closed';
  openedBy: string | null;
}
export function shellSnippet(lang: string | null, command: string): ShellSnippet | null {
  switch (lang?.toLowerCase()) {
    case 'sh':
    case 'bash':
    case 'zsh':
      return { command, interpreter: lang.toLowerCase() as ShellInterpreter };
    case 'shell':
    case 'shellscript':
      return { command, interpreter: 'sh' };
    default:
      return null;
  }
}
export function runSettled(run: ShellRun): boolean {
  return ['completed', 'denied', 'cancelled', 'interrupted'].includes(run.phase);
}
export function runStatus(run: ShellRun): string {
  if (run.problem) return run.problem;
  switch (run.outcome?.kind) {
    case 'success':
      return 'Finished · exit 0';
    case 'failed':
      return `Finished · exit ${run.outcome.code}`;
    case 'signalled':
      return `Stopped · signal ${run.outcome.signal}`;
    case 'timed_out':
      return 'Timed out';
    case 'cancelled':
      return 'Cancelled';
  }
  return {
    prepared: 'Ready for review',
    awaiting_approval: 'Needs your approval',
    awaiting_pane: 'Waiting for a visible pane',
    opening_shell: 'Opening shell',
    reserved: 'Waiting for the empty prompt',
    running: 'Running',
    completed: 'Finished',
    denied: 'Denied',
    cancelled: 'Cancelled',
    interrupted: 'Interrupted · outcome unknown',
  }[run.phase];
}

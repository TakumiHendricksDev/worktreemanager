/**
 * What a review run's state reads as, in the Reviews tab's words.
 *
 * Pure, so the words are decided in one place and the component only lays them out. The maps are
 * keyed by the Rust enums' unions: a status the engine gains and this file does not handle is a
 * type error rather than a blank row, which is the only check a wrong word can get here — there is
 * no JS test runner, and `svelte-check` cannot see a string.
 *
 * The states follow the D1 design's Reviews tab: running, waiting for usage capacity, interrupted,
 * a role failed, consolidated. Staleness, consolidation proper and the finding list are R6's.
 */

import type { ReviewRunView, ReviewStep, RoleProvider, StepStatus } from './ipc/types';

/** The class modifier a state is drawn with. A union, so a misspelt tone cannot reach the CSS. */
export type RunTone = 'info' | 'warn' | 'danger' | 'ok' | 'muted';

export const STEP_WORDS: Record<StepStatus, string> = {
  pending: 'waiting to start',
  running: 'running',
  waiting_capacity: 'paused for usage capacity',
  done: 'done',
  failed: 'failed',
  replaced: 'handed over',
  interrupted: 'cut off',
  cancelled: 'cancelled',
  skipped: 'skipped',
};

export const STEP_TONE: Record<StepStatus, RunTone> = {
  pending: 'muted',
  running: 'info',
  waiting_capacity: 'warn',
  done: 'ok',
  failed: 'danger',
  replaced: 'muted',
  interrupted: 'warn',
  cancelled: 'muted',
  skipped: 'muted',
};

const PROVIDER_NAMES: Record<RoleProvider, string> = {
  claude: 'Claude',
  codex: 'Codex',
};

/** A provider id as its name, for the ids a hold carries as plain strings. */
export function providerName(id: string): string {
  return id in PROVIDER_NAMES ? PROVIDER_NAMES[id as RoleProvider] : id;
}

/**
 * A message split into plain text and `code`, so a role or skill name the engine quoted with
 * backticks is drawn as code rather than with the backticks showing.
 */
export function codeSpans(text: string): { text: string; code: boolean }[] {
  return text
    .split('`')
    .map((part, index) => ({ text: part, code: index % 2 === 1 }))
    .filter((part) => part.text.length > 0);
}

/** "Claude sonnet", "Codex": who runs a role. */
export function runsOn(step: Pick<ReviewStep, 'provider' | 'model'>): string {
  const name = PROVIDER_NAMES[step.provider];
  return step.model ? `${name} ${step.model}` : name;
}

/** How many findings a role handed in, or null before it handed in anything. */
export function findingCount(step: ReviewStep): number | null {
  if (step.result?.kind !== 'findings') return null;
  return step.result.payload.findings.length;
}

/** The one line beside a role: its count, what it is doing, or why it stopped. */
export function stepLine(view: ReviewRunView, step: ReviewStep): string {
  const count = findingCount(step);
  if (step.status === 'done' && count !== null) {
    return `done · ${count} finding${count === 1 ? '' : 's'}`;
  }
  if (step.status === 'running') {
    const live = [...view.invocations]
      .reverse()
      .find((invocation) => invocation.step === step.id);
    const attempt = live && live.attempt > 1 ? ` · attempt ${live.attempt}` : '';
    return live?.progress ? `${live.progress}${attempt}` : `running${attempt}`;
  }
  const words = STEP_WORDS[step.status];
  return step.detail ? `${words}: ${step.detail}` : words;
}

/** The roles worth listing: a skipped role is said in a sentence, not a row. */
export function listedSteps(view: ReviewRunView): ReviewStep[] {
  return view.steps.filter((step) => step.status !== 'skipped');
}

/** A Unix-seconds reset as a time of day, or null when the provider did not say. */
export function resetTime(resetsAt: number | null): string | null {
  if (resetsAt === null) return null;
  return new Date(resetsAt * 1000).toLocaleTimeString([], {
    hour: 'numeric',
    minute: '2-digit',
  });
}

export interface RunHeadline {
  eyebrow: string;
  title: string;
  prose: string;
  tone: RunTone;
}

/** The block at the top of the Reviews tab for a run. */
export function headline(view: ReviewRunView): RunHeadline {
  const { state } = view.flow;
  const steps = listedSteps(view);
  const done = steps.filter((s) => s.status === 'done').length;
  const total = steps.filter((s) => s.status !== 'replaced').length;
  const kept = `${done} of ${total} roles had finished; their results are kept.`;
  const tree = view.snapshot ? view.snapshot.headSha.slice(0, 7) : null;

  if (state.end?.kind === 'cancelled') {
    return {
      eyebrow: 'Stopped',
      title: 'This review was stopped',
      prose: `${kept} Run it again to review the current state of the worktree.`,
      tone: 'muted',
    };
  }
  if (state.end?.kind === 'failed') {
    return {
      eyebrow: 'Could not run',
      title: 'This review could not run',
      prose: state.end.reason,
      tone: 'danger',
    };
  }
  if (state.hold?.kind === 'waiting_capacity') {
    const paused = steps.find((s) => s.status === 'waiting_capacity');
    const at = resetTime(state.hold.resetsAt);
    return {
      eyebrow: 'Waiting for usage capacity',
      title: `${paused?.role ?? 'A role'} is paused: ${providerName(state.hold.provider)} limit reached`,
      prose: at
        ? `The limit resets at ${at}. ${kept} The paused role runs again by itself then, if wtm is open. Nothing switches provider by itself, and wtm never uses an API key.`
        : `The provider did not say when its limit resets. ${kept} Resume when it has.`,
      tone: 'warn',
    };
  }
  if (state.hold?.kind === 'interrupted') {
    const cut = steps.filter((s) => s.status === 'interrupted').map((s) => s.role);
    return {
      eyebrow: 'Interrupted',
      title:
        state.hold.cause === 'quit'
          ? 'wtm quit during this review'
          : 'wtm stopped unexpectedly during this review',
      prose: `${kept} ${
        cut.length > 0
          ? `${cut.join(', ')} ${cut.length === 1 ? 'was' : 'were'} cut off, and `
          : ''
      }nothing was sent again on relaunch. Resume runs what is left against the same snapshot.`,
      tone: 'warn',
    };
  }
  if (state.end === null && state.phase === 'consolidated') {
    const failed = steps.filter((s) => s.status === 'failed');
    if (failed.length > 0) {
      return {
        eyebrow: 'A role failed',
        title: `${failed.map((s) => s.role).join(', ')} did not finish`,
        prose: `${kept} The review is incomplete until you retry the failed ${
          failed.length === 1 ? 'role' : 'roles'
        }.`,
        tone: 'danger',
      };
    }
    const findings = steps.reduce((sum, s) => sum + (findingCount(s) ?? 0), 0);
    return {
      eyebrow: 'Reviewed',
      title:
        findings === 0
          ? 'No findings'
          : `${findings} finding${findings === 1 ? '' : 's'} from ${done} role${done === 1 ? '' : 's'}`,
      prose: `Snapshot ${tree ?? ''} against ${view.review.base}. Each role's results are as it handed them in; grouping and staleness arrive with the full Reviews tab.`,
      tone: findings === 0 ? 'ok' : 'info',
    };
  }
  if (state.end?.kind === 'done') {
    return {
      eyebrow: 'Kept',
      title: 'An earlier review',
      prose: `${kept}`,
      tone: 'muted',
    };
  }
  return {
    eyebrow: state.phase === 'reviewing' ? 'Running' : 'Preparing',
    title: tree
      ? `Reviewing snapshot ${tree} + uncommitted`
      : 'Taking a snapshot of the worktree',
    prose:
      'Reviewers read a snapshot copy, so you can keep editing. Edits after the snapshot are not in this review.',
    tone: 'info',
  };
}

/** Whether Resume does something for this run. */
export function canResume(view: ReviewRunView): boolean {
  const { state } = view.flow;
  if (state.end !== null) return false;
  if (state.hold?.kind === 'interrupted' || state.hold?.kind === 'waiting_capacity')
    return true;
  return (
    !view.running &&
    state.phase === 'consolidated' &&
    view.steps.some((s) => s.status === 'failed' || s.status === 'interrupted')
  );
}

/**
 * What Resume is called for this run. On a capacity pause it runs the paused role before the reset
 * the provider named, which may meet the same limit; the run already resumes by itself at the reset.
 */
export function resumeWords(view: ReviewRunView): string {
  const { state } = view.flow;
  if (state.hold?.kind === 'waiting_capacity') return 'Try now';
  if (state.phase === 'consolidated') return 'Retry failed roles';
  return 'Resume';
}

/** Whether the run can be stopped: it has not ended. */
export function canStop(view: ReviewRunView): boolean {
  return view.flow.state.end === null && (view.running || view.flow.state.hold !== null);
}

/** Whether a new run of the same worktree would start rather than focus this one. */
export function canRunAgain(view: ReviewRunView): boolean {
  const { state } = view.flow;
  return (
    state.end !== null ||
    (!view.running && state.hold === null && state.phase === 'consolidated')
  );
}

/** The roles the plan skipped, as one sentence, or null. */
export function skippedLine(view: ReviewRunView): string | null {
  const skipped = view.steps.filter((s) => s.status === 'skipped').map((s) => s.role);
  if (skipped.length === 0) return null;
  return `Skipped, because none of their paths changed: ${skipped.join(', ')}.`;
}

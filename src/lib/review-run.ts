/**
 * What a review run's state reads as, in the Reviews tab's words.
 *
 * Pure, so the words are decided in one place and the component only lays them out. The maps are
 * keyed by the Rust enums' unions: a status the engine gains and this file does not handle is a
 * type error rather than a blank row, which is the only check a wrong word can get here — there is
 * no JS test runner, and `svelte-check` cannot see a string.
 *
 * The states follow the D1 design's Reviews tab, all nine: not reviewed (the component's own),
 * running, waiting for usage capacity, interrupted, a role failed, stale, reviewed (clean, with any
 * skipped check said so), can't start over billing (a start refusal, see `startProblem`), and
 * can't run for want of a skill.
 */

import { fixHeadline } from './local-fix';
import type {
  CheckRun,
  CheckStatus,
  CreditProblem,
  ProviderSwitch,
  QueuedSeat,
  FindingRecord,
  ReviewRunView,
  ReviewStaleness,
  ReviewStep,
  RoleProvider,
  Screenshot,
  SkillProblem,
  StepStatus,
} from './ipc/types';
import { isWtmError } from './ipc/types';
import type { FindingClass } from './knowledge';

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

/** "next in line", "2nd in line": where a queued role is among those waiting for the same cap. */
export function inLine(position: number): string {
  if (position <= 1) return 'next in line';
  const tens = position % 100;
  const suffix =
    tens >= 11 && tens <= 13
      ? 'th'
      : (({ 1: 'st', 2: 'nd', 3: 'rd' } as Record<number, string>)[position % 10] ?? 'th');
  return `${position}${suffix} in line`;
}

/** The seat a role waits with for a run-session place, if it waits for one. */
export function queuedFor(view: ReviewRunView, step: ReviewStep): QueuedSeat | null {
  return view.queued.find((q) => q.seat.step === step.id) ?? null;
}

/** The one line beside a role: its count, what it is doing, or why it stopped. */
export function stepLine(view: ReviewRunView, step: ReviewStep): string {
  const queued = queuedFor(view, step);
  if (queued) return `${queued.words} · ${inLine(queued.position)}`;
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

/**
 * Whether the plan left a role out because none of its paths changed. Such a role never ran, which
 * is what tells it from one you finished the review without at a usage limit: that one ran into
 * the limit first, so it has an attempt on record.
 */
export function plannedOut(view: ReviewRunView, step: ReviewStep): boolean {
  return step.status === 'skipped' && !view.invocations.some((i) => i.step === step.id);
}

/** The roles worth listing: a role the plan left out is said in a sentence, not a row. */
export function listedSteps(view: ReviewRunView): ReviewStep[] {
  return view.steps.filter((step) => !plannedOut(view, step));
}

/** Roles you finished the review without, at a usage limit. */
export function leftOut(view: ReviewRunView): ReviewStep[] {
  return view.steps.filter((step) => step.status === 'skipped' && !plannedOut(view, step));
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

/** The block at the top of the Reviews tab for a run. `stale` is the run's last staleness read. */
export function headline(
  view: ReviewRunView,
  stale: ReviewStaleness | null = null,
): RunHeadline {
  const { state } = view.flow;
  const steps = listedSteps(view);
  const done = steps.filter((s) => s.status === 'done').length;
  const total = steps.filter((s) => s.status !== 'replaced').length;
  const kept = `${done} of ${total} roles had finished; their results are kept.`;
  const tree = view.snapshot ? view.snapshot.headSha.slice(0, 7) : null;

  // A local fix is the run's news while it is being written or waits to be sent.
  const fixing = fixHeadline(view);
  if (fixing) return fixing;
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
    const provider = providerName(state.hold.provider);
    const at = resetTime(state.hold.resetsAt);
    const recheck = view.capacity?.recheck ? ` ${view.capacity.recheck}` : '';
    return {
      eyebrow: 'Waiting for usage capacity',
      title: `${paused?.role ?? 'A role'} is paused: ${provider} limit reached`,
      prose: at
        ? `The limit resets at ${at}. ${kept} The paused role resumes automatically if wtm is open and the Mac is awake; after a sleep or a relaunch, wtm asks ${provider} again before it runs.${recheck}`
        : `${provider} did not say when its limit resets, so wtm does not guess. ${kept}${recheck} Try it when you think the limit has reset.`,
      tone: 'warn',
    };
  }
  if (state.hold?.kind === 'blocked' && state.hold.cause === 'env') {
    const waiting = view.pendingStart?.request.roles ?? [];
    const services = view.pendingStart?.request.services.map((s) => s.label) ?? [];
    return {
      eyebrow: 'Blocked: environment not started',
      title: `${waiting.length > 0 ? waiting.join(', ') : 'A runtime role'} ${
        waiting.length > 1 ? 'wait' : 'waits'
      } for ${services.length > 0 ? services.join(', ') : 'its services'}`,
      prose: `A runtime role exercises the running app, and ${
        services.length === 1 ? 'its service is' : 'the services it needs are'
      } not answering. ${kept} Start environment and run starts them, waits for each to answer its health check, then runs it; nothing has run, and nothing reads as passed.`,
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
    const blocked = failed.find((s) => s.skillProblem !== null);
    if (blocked?.skillProblem) {
      const problem = blocked.skillProblem;
      const provider = PROVIDER_NAMES[problem.provider];
      return problem.kind === 'missing'
        ? {
            eyebrow: "Can't run: recipe",
            title: `\`/${problem.skill}\` isn't available to ${provider} here`,
            prose: `The \`${problem.role}\` role names the skill, but ${provider}'s skill list for this worktree doesn't include it. Providers don't share skills, so wtm checks each role against its own. Edit the recipe to change the role's skills or provider, then Run again: a review keeps the recipe it started with, so Retry asks ${provider} again, which helps once the skill is installed for it. ${kept}`,
            tone: 'danger',
          }
        : {
            eyebrow: "Can't run: recipe",
            title: `${provider} never listed its skills`,
            prose: `The \`${problem.role}\` role names skills, and ${provider} did not say which it has, so wtm could not check them. Edit the recipe, then Run again: a review keeps the recipe it started with. ${kept}`,
            tone: 'danger',
          };
    }
    const live = view.findings.filter((f) => f.dismissed === null);
    const behind = live.filter((f) => stale?.staleFindings.includes(f.id));
    if (stale?.pullRequestHead && view.snapshot) {
      // A teammate's pull request moved on: the author pushed, and the review read the old head.
      const now = stale.pullRequestHead.slice(0, 7);
      const was = view.snapshot.headSha.slice(0, 7);
      const number = view.pullRequest ? `#${view.pullRequest.number}` : 'the pull request';
      const holding = live.length - behind.length;
      return {
        eyebrow: 'Stale',
        title: `New commits on ${number} since this review`,
        prose: stale.pullRequestHeadHere
          ? `The author pushed ${now}; this review read ${was}. ${
              behind.length === 1 ? '1 finding is' : `${behind.length} findings are`
            } on files the new commits touch${
              holding > 0
                ? `, and ${holding === 1 ? '1 still holds' : `${holding} still hold`}`
                : ''
            }. Re-review changes moves the review worktree to ${now} and reruns the roles those files affect.`
          : `The author pushed ${now}; this review read ${was}. wtm has not fetched the new commits, so it cannot tell which files they touch, and every finding is stale until Re-review changes brings them in and reruns the roles they affect.`,
        tone: 'warn',
      };
    }
    if (stale && !stale.changed && behind.length > 0) {
      // The latest snapshot is current, and a role that was not run again still has a report
      // from before the change: the review is only partly about the code as it is.
      const files = [
        ...new Set(behind.map((f) => f.location?.path).filter((p): p is string => !!p)),
      ];
      return {
        eyebrow: 'Stale',
        title: `${behind.length === 1 ? '1 finding predates' : `${behind.length} findings predate`} the latest change`,
        prose: `${
          files.length > 0 ? files.map((path) => `\`${path}\``).join(', ') : 'Its file'
        } changed after the role that reported ${behind.length === 1 ? 'it' : 'them'} ran, and that role was not run again. ${
          stale.affected.length > 0
            ? `Rerun affected runs ${stale.affected.map((role) => `\`${role}\``).join(', ')} against the latest snapshot.`
            : ''
        }`,
        tone: 'warn',
      };
    }
    if (stale?.changed) {
      const count = stale.paths.length;
      const files = stale.paths.slice(0, 3).map((path) => `\`${path}\``);
      const more = count > 3 ? ` and ${count - 3} more` : '';
      const head =
        stale.headSha && view.snapshot && stale.headSha !== view.snapshot.headSha
          ? `New commit ${stale.headSha.slice(0, 7)} `
          : 'Edits since the snapshot ';
      const staleCount = behind.length;
      const holding = live.length - staleCount;
      return {
        eyebrow: 'Stale',
        title: stale.complete
          ? `${count} file${count === 1 ? '' : 's'} changed after this review`
          : 'More files changed after this review than git could list',
        prose: `${head}touched ${files.join(', ')}${more}. ${
          staleCount === 1 ? '1 finding' : `${staleCount} findings`
        } and the overall result are stale.${
          holding > 0
            ? ` ${holding === 1 ? '1 finding' : `${holding} findings`} on untouched files still ${holding === 1 ? 'holds' : 'hold'}.`
            : ''
        }`,
        tone: 'warn',
      };
    }
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
    const findings = view.findings.filter((f) => f.dismissed === null).length;
    const without = leftOut(view).map((s) => `\`${s.role}\``);
    const missingRoles =
      without.length > 0
        ? ` You finished it without ${without.join(', ')} at ${without.length === 1 ? 'its' : 'their'} usage limit.`
        : '';
    const checks = checkLines(view, stale);
    const unproven = checks.filter((c) => c.state !== 'passed');
    const passed = checks.filter((c) => c.state === 'passed').map((c) => c.label);
    const rules = view.applied.rules.map((r) => ruleLabel(r.rule, r.revision));
    const roles = `${done} role${done === 1 ? '' : 's'}`;
    const ran = `${roles}${rules.length > 0 ? `, rules ${rules.join(', ')} applied` : ''}.`;
    const proved = passed.length > 0 ? ` ${passed.join(' and ')} passed.` : '';
    const said = (c: CheckLine) =>
      `${c.label} ${CHECK_SENTENCE[c.state]}${c.detail ? ` (${c.detail})` : ''}`;
    const broke = unproven.filter((c) => CHECK_TONE[c.state] === 'danger');
    const missing = unproven.filter((c) => CHECK_TONE[c.state] !== 'danger');
    const caveat =
      (broke.length > 0 ? ` ${broke.map(said).join('; ')}.` : '') +
      (missing.length > 0
        ? ` ${missing.map(said).join('; ')}, so this is not a runtime pass.`
        : '');
    return {
      eyebrow: findings === 0 ? 'Clean' : 'Reviewed',
      title:
        findings === 0
          ? 'No findings'
          : `${findings} finding${findings === 1 ? '' : 's'} from ${roles}`,
      prose: `${ran}${proved}${caveat}${missingRoles}`,
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
  if (view.pullRequest) {
    // A teammate's pull request: its review worktree holds the head and nothing of yours.
    return {
      eyebrow: state.phase === 'reviewing' ? 'Running' : 'Preparing',
      title: tree
        ? `Reviewing #${view.pullRequest.number} at ${tree}`
        : `Taking a snapshot of #${view.pullRequest.number}`,
      prose:
        "Reviewers read a snapshot copy of the pull request's review worktree, against its base. They only read: nothing is posted to GitHub.",
      tone: 'info',
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

/**
 * Whether the run can be stopped: it has not ended. A run that is fixing is stopped by Stop fix,
 * which ends the fix and keeps the review; stopping the review would end both.
 */
export function canStop(view: ReviewRunView): boolean {
  return (
    view.flow.state.end === null &&
    view.flow.state.phase !== 'fixing' &&
    (view.running || view.flow.state.hold !== null)
  );
}

/** Whether the fix being written can be stopped. */
export function canStopFix(view: ReviewRunView): boolean {
  return view.flow.state.end === null && view.flow.state.phase === 'fixing';
}

/**
 * The skill problem that stopped a finished review's role: "can't run: recipe". Its Edit recipe and
 * Move role buttons open Settings › Recipes at that role.
 */
export function recipeProblem(view: ReviewRunView): SkillProblem | null {
  const { state } = view.flow;
  if (state.end !== null || state.phase !== 'consolidated') return null;
  return (
    view.steps.find((s) => s.status === 'failed' && s.skillProblem !== null)
      ?.skillProblem ?? null
  );
}

/**
 * Whether a new run of the same worktree would start rather than focus this one. A fix of your own
 * work counts as settled: it waits for no route, so reviewing the fix is the next thing to do. One
 * of a teammate's pull request waits for the route you approve, and starting over would leave it.
 */
export function canRunAgain(view: ReviewRunView): boolean {
  const { state } = view.flow;
  return (
    state.end !== null ||
    (!view.running &&
      state.hold === null &&
      (state.phase === 'consolidated' ||
        (state.phase === 'fix_ready' && view.pullRequest === null)))
  );
}

/** The roles the plan skipped, as one sentence, or null. */
export function skippedLine(view: ReviewRunView): string | null {
  const skipped = view.steps.filter((s) => plannedOut(view, s)).map((s) => s.role);
  if (skipped.length === 0) return null;
  return `Skipped, because none of their paths changed: ${skipped.join(', ')}.`;
}

// ── findings ──

/** The sections findings are grouped into, in the design's order. A union, as `RunTone` is. */
export type FindingGroup = FindingClass['kind'];

export const GROUP_ORDER: FindingGroup[] = [
  'confirmed_bug',
  'rule_violation',
  'conflict',
  'suggestion',
];

export const GROUP_WORDS: Record<FindingGroup, string> = {
  confirmed_bug: 'Confirmed bugs',
  rule_violation: 'Rule violations',
  conflict: 'Conflicts to resolve',
  suggestion: 'Suggestions',
};

/** The findings still standing, grouped by class, empty groups left out. Dismissed ones are not here. */
export function findingGroups(
  findings: readonly FindingRecord[],
): { group: FindingGroup; findings: FindingRecord[] }[] {
  return GROUP_ORDER.map((group) => ({
    group,
    findings: findings.filter((f) => f.dismissed === null && f.class.kind === group),
  })).filter((section) => section.findings.length > 0);
}

/** `R-14 v2`, the way a reviewer is told to cite a rule. */
export function ruleLabel(rule: number, revision: number): string {
  return `R-${rule} v${revision}`;
}

/** Where a finding is, as a link reads: `src/export.rs:40–52`, the file alone for a whole file. */
export function findingPlace(finding: FindingRecord): string | null {
  const at = finding.location;
  if (!at) return null;
  if (!finding.lines) return at.path;
  return at.startLine === at.endLine
    ? `${at.path}:${at.startLine}`
    : `${at.path}:${at.startLine}–${at.endLine}`;
}

/** Who said it: `correctness · Codex gpt-6`, once per role. */
export function findingSources(finding: FindingRecord): string[] {
  const seen = new Set<string>();
  const out: string[] = [];
  for (const { source } of finding.evidence) {
    const line = `${source.role} · ${runsOn(source)}`;
    if (!seen.has(line)) {
      seen.add(line);
      out.push(line);
    }
  }
  return out;
}

/** The finding's evidence of its class: what confirmed the bug, which rule it breaks, which sources disagree. */
export function classLine(finding: FindingRecord, view: ReviewRunView): string | null {
  const cls = finding.class;
  switch (cls.kind) {
    case 'confirmed_bug':
      return `Verified: ${cls.verification}`;
    case 'rule_violation': {
      const applied = view.applied.rules.find(
        (r) => r.rule === cls.rule && r.revision === cls.revision,
      );
      const label = ruleLabel(cls.rule, cls.revision);
      return applied
        ? `Rule ${label} (${applied.kind}): "${applied.text}"`
        : `Rule ${label}, which this review's roles were not given: check the citation`;
    }
    case 'conflict':
      return `Between ${cls.between.join(' and ')}. The reviewer did not pick one.`;
    case 'suggestion':
      return null;
  }
}

// ── checks ──

/** A configured check's state in this run. `not_run` has no record at all, which is never a pass. */
export type CheckState = CheckStatus | 'not_run';

export const CHECK_WORDS: Record<CheckState, string> = {
  passed: 'passed',
  failed: 'failed',
  timed_out: 'timed out',
  skipped: 'skipped',
  error: 'could not run',
  blocked: 'blocked',
  not_run: 'not run',
};

/** The same states as the verb of a sentence: "unit tests failed", "e2e was skipped". */
export const CHECK_SENTENCE: Record<CheckState, string> = {
  passed: 'passed',
  failed: 'failed',
  timed_out: 'timed out',
  skipped: 'was skipped',
  error: 'could not run',
  blocked: 'was blocked',
  not_run: 'was not run',
};

export const CHECK_TONE: Record<CheckState, RunTone> = {
  passed: 'ok',
  failed: 'danger',
  timed_out: 'danger',
  skipped: 'warn',
  error: 'danger',
  blocked: 'warn',
  not_run: 'muted',
};

export interface CheckLine {
  id: string;
  label: string;
  state: CheckState;
  /** Why it was skipped or could not run, or how it failed. */
  detail: string | null;
  run: CheckRun | null;
  stale: boolean;
}

/**
 * Every check the run's roles may ask for, with its latest record in this run, then any record of a
 * check no longer configured. A check with no record says "not run"; nothing becomes a pass by
 * being absent.
 */
export function checkLines(
  view: ReviewRunView,
  stale: ReviewStaleness | null,
): CheckLine[] {
  const latest = (id: string): CheckRun | null =>
    [...view.checks].reverse().find((run) => run.check === id) ?? null;
  const line = (id: string, label: string): CheckLine => {
    const run = latest(id);
    const state: CheckState = run ? run.status : 'not_run';
    // A blocked record's detail begins "blocked: …", which the line already says.
    const detail =
      run === null
        ? null
        : run.status === 'failed'
          ? `exit ${run.exitCode ?? '?'}`
          : run.status === 'blocked'
            ? (run.detail?.replace(/^blocked: /, '') ?? null)
            : run.detail;
    return {
      id,
      label,
      state,
      detail,
      run,
      stale: run !== null && (stale?.staleChecks.includes(run.id) ?? false),
    };
  };
  const listed = view.expectedChecks.map((choice) => line(choice.id, choice.label));
  const extra = [...new Set(view.checks.map((run) => run.check))]
    .filter((id) => !view.expectedChecks.some((choice) => choice.id === id))
    .map((id) => line(id, id));
  return [...listed, ...extra];
}

// ── starting ──

/** Why a start was refused, as the Reviews tab draws it. */
export type StartProblem =
  | { kind: 'billing'; providers: string[]; message: string }
  | { kind: 'credits'; roles: CreditProblem[]; message: string }
  | { kind: 'other'; message: string };

/** Read a refused start, rerun or resume. A billing refusal is its own state in the design. */
export function startProblem(error: unknown): StartProblem {
  if (isWtmError(error) && error.kind === 'billing') {
    const detail = error.detail as { providers?: unknown } | null;
    const providers = Array.isArray(detail?.providers)
      ? detail.providers.filter((p): p is string => typeof p === 'string')
      : [];
    return { kind: 'billing', providers, message: error.message };
  }
  if (isWtmError(error) && error.kind === 'credits') {
    const detail = error.detail as { roles?: unknown } | null;
    const roles = Array.isArray(detail?.roles) ? (detail.roles as CreditProblem[]) : [];
    return { kind: 'credits', roles, message: error.message };
  }
  if (isWtmError(error)) return { kind: 'other', message: error.message };
  return { kind: 'other', message: error instanceof Error ? error.message : String(error) };
}

/**
 * When the run's latest round started and how long it took, as `4m 12s`: the attempts that read
 * its current snapshot, so a rerun is timed on its own rather than from the first round with the
 * time between them counted. The length is null while running.
 */
export function latestRound(view: ReviewRunView): {
  startedAt: number;
  length: string | null;
} {
  const round = view.invocations.filter(
    (i) => view.review.snapshot === null || i.snapshot === view.review.snapshot,
  );
  const starts = round.map((i) => i.startedAt);
  const startedAt = starts.length > 0 ? Math.min(...starts) : view.flow.startedAt;
  const ends = round.map((i) => i.endedAt).filter((at): at is number => at !== null);
  if (view.running || ends.length === 0) return { startedAt, length: null };
  const seconds = Math.max(0, Math.round((Math.max(...ends) - startedAt) / 1000));
  const minutes = Math.floor(seconds / 60);
  return {
    startedAt,
    length: minutes > 0 ? `${minutes}m ${seconds % 60}s` : `${seconds}s`,
  };
}

// ── the environment a review runs against ──

/**
 * Which services answered when a check ran, and where: `web at http://localhost:18431/healthz`.
 * Null for a check that needed none, or for one that did not run.
 */
export function servicesLine(run: CheckRun | null): string | null {
  if (!run || run.services.length === 0 || run.status === 'blocked') return null;
  const up = run.services.filter((s) => s.state === 'healthy');
  if (up.length === 0) return null;
  return `with ${up.map((s) => (s.url ? `${s.service} at ${s.url}` : s.service)).join(', ')} answering`;
}

/** The screenshots a finding's reports cite as `s<id>`, in the order they are cited. */
export function citedScreenshots(
  finding: FindingRecord,
  shots: Screenshot[],
): Screenshot[] {
  const ids = finding.evidence
    .flatMap((e) => e.reported.evidence ?? [])
    .map((line) => /^\s*s(\d+)\s*$/.exec(line)?.[1])
    .filter((id): id is string => id !== undefined)
    .map(Number);
  return [...new Set(ids)]
    .map((id) => shots.find((shot) => shot.id === id))
    .filter((shot): shot is Screenshot => shot !== undefined);
}

// ── capacity: switches and the queue ──

/** Why a role's work moved, in the Reviews tab's words. */
export const SWITCH_WORDS: Record<ProviderSwitch['reason'], string> = {
  limit: 'its provider reached a usage limit',
  failure: 'it failed after its retries',
  once: 'you ran it once on another provider at a limit',
};

/** The switch that put `step` where it is, if its work came from another session. */
export function switchInto(view: ReviewRunView, step: ReviewStep): ProviderSwitch | null {
  return [...view.switches].reverse().find((s) => s.toStep === step.id) ?? null;
}

/** "From `correctness` on Codex, because its provider reached a usage limit · by your config". */
export function switchLine(change: ProviderSwitch): string {
  const from = change.handover.from;
  const who = change.by === 'config' ? 'by its fallback chain in your config' : 'by you';
  return `From \`${from.role}\` on ${runsOn(from)}, because ${SWITCH_WORDS[change.reason]} · ${who}`;
}

/** The run's roles waiting for a run-session place, as one sentence, or null. */
export function queueLine(view: ReviewRunView): string | null {
  if (view.queued.length === 0) return null;
  const roles = view.queued.map((q) => `\`${q.seat.role}\` (${q.words})`);
  return `Waiting for a run-session place under your caps: ${roles.join(', ')}. ${
    view.queued.length === 1 ? 'It starts' : 'They start'
  } as soon as a session closes, oldest first.`;
}

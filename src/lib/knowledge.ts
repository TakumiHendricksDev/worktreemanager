/**
 * Review knowledge: your private rules and the repository's instruction sources.
 *
 * The types mirror `wtm_core::flow::rule` and `src-tauri/src/knowledge.rs` as serde writes them,
 * kept here beside the helpers rather than in `ipc/types.ts` the way `open-design.ts` keeps its
 * own, because nothing outside Knowledge and the Save as rule dialog reads them. The contract tests
 * in `knowledge.rs` pin the spellings these rely on.
 *
 * Plain, beside `work.ts`, for the reason that file gives: with no JS test runner, logic reachable
 * only from inside a component cannot be reasoned about in isolation.
 */

export type RuleKind = 'required' | 'guidance';
export type RuleOrigin = 'human' | 'agent' | 'imported';
export type RuleStatus = 'active' | 'disabled' | 'superseded';
export type RuleApproval = 'pending' | 'approved' | 'rejected';
export type ExampleStance = 'prefer' | 'avoid';

export interface RuleExample {
  stance: ExampleStance;
  text: string;
}

export interface RuleScope {
  areas: string[];
  /** Globs relative to the repository root. */
  paths: string[];
}

export interface RuleDraft {
  kind: RuleKind;
  text: string;
  rationale: string;
  scope: RuleScope;
  examples: RuleExample[];
  references: string[];
}

export interface Location {
  path: string;
  startLine: number;
  endLine: number;
}

export type RuleSource =
  | { kind: 'written' }
  | { kind: 'finding'; flow: number; finding: number; location: Location | null }
  | { kind: 'proposal'; flow: number; role: string; finding: number | null }
  | { kind: 'replacement'; replaces: number };

export interface RuleRevision {
  number: number;
  draft: RuleDraft;
  at: number;
}

export interface RuleView {
  id: number;
  project: string;
  origin: RuleOrigin;
  source: RuleSource;
  status: RuleStatus;
  approval: RuleApproval;
  supersededBy: number | null;
  /** Oldest first, never empty. */
  revisions: RuleRevision[];
  updatedAt: number;
  usage: { reviews: number; lastAt: number; lastSubject: string | null } | null;
  /** The work item the rule came out of, such as `APP-8701`. */
  sourceSubject: string | null;
}

export type RuleChange =
  | { kind: 'revise'; draft: RuleDraft }
  | { kind: 'approve' }
  | { kind: 'approve_revised'; draft: RuleDraft }
  | { kind: 'reject' }
  | { kind: 'disable' }
  | { kind: 'enable' };

export type Severity = 'minor' | 'notable' | 'blocking';

export type FindingClass =
  | { kind: 'confirmed_bug'; verification: string }
  | { kind: 'rule_violation'; rule: number; revision: number }
  | { kind: 'conflict'; between: string[] }
  | { kind: 'suggestion' };

/** A review finding, as `wtm_core::flow::Finding` serializes. */
export interface Finding {
  id: number;
  flow: number;
  class: FindingClass;
  severity: Severity;
  location: Location | null;
  title: string;
  rationale: string;
  requirement: string | null;
  treeSha: string;
  dismissed: { reason: string; at: number } | null;
}

export interface InstructionSources {
  patterns: string[];
  /** False when these are the defaults because the project names none. */
  configured: boolean;
  documents: { path: string; pattern: string; truncated: boolean }[];
  unmatched: string[];
  skipped: { path: string; reason: string }[];
  /** Git's listing was cut short, so a source may exist and not be found. */
  incomplete: boolean;
}

/**
 * The Knowledge list's filters. A union, like `ReviewStatus` in `work.ts`, because the chip and
 * its count are built from it and a typo would otherwise be a filter that silently matches nothing.
 */
export type RuleFilter = 'active' | 'proposed' | 'disabled' | 'superseded';

export const FILTERS: { id: RuleFilter; label: string; empty: string }[] = [
  { id: 'active', label: 'Active', empty: 'No active rules.' },
  { id: 'proposed', label: 'Proposed', empty: 'No proposals waiting for you.' },
  { id: 'disabled', label: 'Disabled', empty: 'No disabled rules.' },
  { id: 'superseded', label: 'Superseded', empty: 'No replaced rules.' },
];

export const KIND_WORDS: Record<RuleKind, { label: string; help: string }> = {
  guidance: {
    label: 'Guidance',
    help: 'informs planning and review; not a violation on its own',
  },
  required: { label: 'Required', help: 'breaking it is reported as a rule finding' },
};

export const SEVERITY_WORDS: Record<Severity, string> = {
  minor: 'Minor',
  notable: 'Notable',
  blocking: 'Blocking',
};

/** Which filter a rule is listed under. A rejected proposal is under none: it never applied. */
export function filterOf(rule: RuleView): RuleFilter | null {
  if (rule.approval === 'rejected') return null;
  if (rule.approval === 'pending') return 'proposed';
  return rule.status;
}

export function countByFilter(rules: readonly RuleView[]): Record<RuleFilter, number> {
  const counts: Record<RuleFilter, number> = {
    active: 0,
    proposed: 0,
    disabled: 0,
    superseded: 0,
  };
  for (const rule of rules) {
    const filter = filterOf(rule);
    if (filter) counts[filter] += 1;
  }
  return counts;
}

/** The id a finding cites and a reviewer is told: `R-14`. */
export function ruleLabel(id: number): string {
  return `R-${id}`;
}

export function current(rule: RuleView): RuleRevision {
  // Never empty: the store refuses to read a rule without revisions.
  return rule.revisions[rule.revisions.length - 1]!;
}

/** "frontend", "exports, backend", or "all" for a rule about the whole project. */
export function areaWords(draft: RuleDraft): string {
  if (draft.scope.areas.length > 0) return draft.scope.areas.join(', ');
  return draft.scope.paths.length > 0 ? 'some files' : 'all';
}

/**
 * The detail pane's Scope line, as parts joined by a dot: the repository, each area, and each glob,
 * which is drawn as code.
 */
export function scopeParts(
  draft: RuleDraft,
  projectName: string,
): { text: string; code: boolean }[] {
  const parts = [
    { text: `Repository ${projectName}`, code: false },
    ...draft.scope.areas.map((area) => ({ text: `area ${area}`, code: false })),
    ...draft.scope.paths.map((path) => ({ text: path, code: true })),
  ];
  if (parts.length === 1) parts.push({ text: 'every file', code: false });
  return parts;
}

/** Commas or new lines separate entries in the editor's list fields. */
export function splitList(text: string): string[] {
  return text
    .split(/[,\n]/)
    .map((part) => part.trim())
    .filter((part) => part !== '');
}

/** One entry per line, for the example fields, where a comma is part of the text. */
export function splitLines(text: string): string[] {
  return text
    .split('\n')
    .map((part) => part.trim())
    .filter((part) => part !== '');
}

export function locationText(location: Location): string {
  return location.startLine === location.endLine
    ? `${location.path}:${location.startLine}`
    : `${location.path}:${location.startLine}-${location.endLine}`;
}

export function emptyDraft(): RuleDraft {
  return {
    kind: 'guidance',
    text: '',
    rationale: '',
    scope: { areas: [], paths: [] },
    examples: [],
    references: [],
  };
}

/**
 * The editor's starting point for Save as rule: the finding's title, rationale and file. The
 * backend builds the same draft in `RuleDraft::from_finding` and refuses a dismissed finding
 * there; this returns null for one so the dialog never offers to save it.
 */
export function draftFromFinding(finding: Finding): RuleDraft | null {
  if (finding.dismissed !== null) return null;
  return {
    ...emptyDraft(),
    text: finding.title,
    rationale: finding.rationale,
    scope: { areas: [], paths: finding.location ? [finding.location.path] : [] },
  };
}

/** "Oct 12", in the reader's locale. */
export function dayWords(ms: number): string {
  return new Date(ms).toLocaleDateString(undefined, { month: 'short', day: 'numeric' });
}

/** Who wrote a revision. The first is by the rule's origin; nothing but you edits one after. */
export function revisionAuthor(rule: RuleView, revision: RuleRevision): string {
  if (revision.number > 1) return 'you';
  return ORIGIN_WORDS[rule.origin];
}

const ORIGIN_WORDS: Record<RuleOrigin, string> = {
  human: 'you',
  agent: 'an agent',
  imported: 'imported',
};

/** The detail pane's Origin line: who made the rule, from what, and when. */
export function originWords(rule: RuleView): string {
  const first = rule.revisions[0];
  const when = first ? ` · ${dayWords(first.at)}` : '';
  const on = rule.sourceSubject ? ` on ${rule.sourceSubject}` : '';
  switch (rule.source.kind) {
    case 'written':
      return `You${when}`;
    case 'finding':
      return `You, from a finding${on}${when}`;
    case 'proposal':
      return `Proposed by the ${rule.source.role} role${on}${when}`;
    case 'replacement':
      return `You, replacing ${ruleLabel(rule.source.replaces)}${when}`;
  }
}

/** The list row's last line. */
export function usageWords(rule: RuleView): string {
  if (!rule.usage) return 'Not applied in a review yet';
  const reviews = rule.usage.reviews === 1 ? '1 review' : `${rule.usage.reviews} reviews`;
  return `Applied in ${reviews}`;
}

/** The detail pane's Applied line. */
export function appliedWords(rule: RuleView): string {
  if (!rule.usage)
    return 'Not yet. A review records a rule when one of its roles is given it.';
  const on = rule.usage.lastSubject ? ` on ${rule.usage.lastSubject}` : '';
  return `${usageWords(rule).replace('Applied in ', '')} · last${on}, ${dayWords(rule.usage.lastAt)}`;
}

/** "R-14 · version 2 · active · yours", over the detail pane. */
export function stateWords(rule: RuleView): string {
  const status =
    rule.approval === 'pending'
      ? 'proposed, inactive'
      : rule.approval === 'rejected'
        ? 'rejected'
        : rule.status;
  const whose = rule.origin === 'agent' ? "an agent's" : 'yours';
  return `${ruleLabel(rule.id)} · version ${current(rule).number} · ${status} · ${whose}`;
}

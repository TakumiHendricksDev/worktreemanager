/**
 * Drive the real app's Work view with mocked IPC through every state the Reviews tab draws — the
 * design's nine: not reviewed (with a recipe to pick), running, waiting for usage capacity,
 * interrupted by a quit, a role that failed, stale, clean with a check blocked on its environment,
 * can't start over billing, and can't run for want of a skill — and through a review with findings
 * of every class: their grouping, evidence, checks, applied rules and sources, Dismiss, Save as
 * rule and Rerun affected. Plus E3's two: a runtime role waiting for its environment, with Start
 * environment and run, and a runtime role's finding with its screenshot evidence. Plus C1/C3's: the
 * capacity card under a usage pause (Wait and resume, Run this role once elsewhere with its model
 * picker, the usage line saying "unknown" without figures), a role queued for a run-session place,
 * and a fallback's handover opened under its row. Plus the run inbox, Run Review's request, and
 * Resume's. Plus fixing your own work: Fix findings offers only the findings that name a file, picks
 * what a review would publish, refuses with the app's words where the click was, and sends exactly
 * the picks and the writer; while the writer runs the run says it holds the worktree and offers
 * Stop fix; a ready fix says it is on your branch, opens on its diff, and Review the fix starts a
 * new review.
 *
 * No Tauri, no CLI, no credits. What it proves is the readout's words and wiring against the shapes
 * `review_run` returns; the engine behind them is `src-tauri/tests/review_engine.rs`'s.
 *
 * `WTM_PLAYWRIGHT_MODULE` names a Playwright module, as for `check-work-view-ui.mjs`. Screenshots
 * go to `WTM_SHOTS`, or a directory under the system temp dir, in light and dark.
 */
import assert from 'node:assert/strict';
import { mkdirSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { createServer } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

if (!process.env.WTM_PLAYWRIGHT_MODULE)
    throw new Error('Set WTM_PLAYWRIGHT_MODULE to an existing Playwright module.');
const { chromium } = await import(pathToFileURL(process.env.WTM_PLAYWRIGHT_MODULE).href);
const shots = process.env.WTM_SHOTS || join(tmpdir(), 'wtm-review-run');
mkdirSync(shots, { recursive: true });

/*
 * The page: the real `App`, Work on and showing, over a listing of five worktrees, each with a run
 * in a different state. Calls are recorded on `window.calls`.
 */
const entry = `
import { emit } from '@tauri-apps/api/event';
import { mockIPC, mockWindows } from '@tauri-apps/api/mocks';
const params = new URLSearchParams(location.search);
const prefKey = (k) => 'harness.pref.' + k;
localStorage.setItem(prefKey('ui.notify'), 'off');
localStorage.setItem(prefKey('ui.work_view'), 'on');
localStorage.setItem('wtm.view', 'work');
if (params.get('theme')) localStorage.setItem(prefKey('ui.theme'), params.get('theme'));
document.documentElement.dataset.theme = localStorage.getItem(prefKey('ui.theme')) ?? 'light';
document.documentElement.dataset.palette = 'pine';
document.documentElement.dataset.platform = 'macos';
mockWindows('main');
const project = { id: '/x/webapp', name: 'webapp', root: '/x/webapp', usable: true, problem: null, trust: null };
const id = (dir) => '/x/webapp-wt/' + dir;
const wt = (dir, branch) => ({
  id: id(dir), title: dir, subtitle: branch, path: id(dir), dirname: dir, branch, head: 'a1b2c3d4e5',
  isMain: false, isBare: false, locked: null, prunable: null, dirty: true, untracked: 1,
  staged: 0, ahead: 2, behind: 0, issueKey: null, badges: [], links: [], table: [], env: [],
  browserHome: null });
const worktrees = [
  { ...wt('webapp', 'main'), id: '/x/webapp', path: '/x/webapp', isMain: true },
  wt('running', 'fix/running'),
  wt('waiting', 'fix/waiting'),
  wt('interrupted', 'fix/interrupted'),
  wt('failed-role', 'fix/failed-role'),
  wt('reviewed', 'fix/reviewed'),
  wt('findings', 'fix/findings'),
  wt('stale', 'fix/stale'),
  wt('clean', 'fix/clean'),
  wt('billing', 'fix/billing'),
  wt('skill', 'fix/skill'),
  wt('blocked-role', 'fix/blocked-role'),
  wt('runtime', 'fix/runtime'),
  wt('fixable', 'fix/fixable'),
];
const findings = (n) => ({ kind: 'findings', payload: { findings: Array.from({ length: n }, (_, i) => ({
  title: 'Finding ' + i, severity: 'notable', class: 'suggestion', body: 'x' })) } });
const step = (n, role, provider, status, extra = {}) => ({ id: n, stage: 0, role, provider,
  model: provider === 'claude' ? 'sonnet' : null, profile: 'reviewer', workspace: 'snapshot', status,
  failures: 0, retries: 1, standsInFor: null, detail: null, resetsAt: null, skillProblem: null,
  result: null, ...extra });
const invocation = (n, stepId, role, status, extra = {}) => ({ id: n, step: stepId, attempt: 1, role,
  snapshot: 1, status, detail: null, progress: null, instructionSources: [], startedAt: 0, endedAt: null, ...extra });
const noKnowledge = { rules: [], sources: [], skills: [] };
const run = (flow, worktree, state, steps, invocations = [], inbox = [], running = false, more = {}) => ({
  flow: { id: flow, state: { kind: 'review_mine', end: null, hold: null, ...state }, startedAt: 0, updatedAt: 0 },
  review: { flow, project: '/x/webapp', worktree: id(worktree), worktreePath: id(worktree), recipe: 'standard', base: 'origin/main', snapshot: 1 },
  snapshot: { headSha: 'a1b2c3d4e5f6', treeSha: '9f00aa', baseSha: 'c0ffee11' },
  steps, invocations, inbox, running,
  findings: [], checks: [], expectedChecks: [], applied: noKnowledge,
  pullRequest: null, fixes: [], leases: [], writers: [], screenshots: [], pendingStart: null,
  queued: [], holding: [], switches: [], capacity: null, ...more });
const source = (role, provider, invocationId, ordinal = 0) => ({ step: invocationId, invocation: invocationId,
  ordinal, role, provider, model: provider === 'claude' ? 'sonnet' : 'gpt-6', workspace: 'snapshot', treeSha: '9f00aa' });
const reported = (title, cls, extra = {}) => ({ title, severity: 'notable', class: cls, body: 'Why it matters.', ...extra });
const record = (findingId, flow, cls, title, rationale, location, evidence, extra = {}) => ({
  id: findingId, flow, class: cls, severity: 'notable', location, title, rationale, requirement: null,
  treeSha: '9f00aa', dismissed: null, lines: true, evidence, ...extra });
const at = (path, startLine, endLine = startLine) => ({ path, startLine, endLine });
const check = (checkId, name, status, extra = {}) => ({ id: checkId, check: name, role: 'correctness', status,
  detail: null, exitCode: status === 'passed' ? 0 : status === 'failed' ? 1 : null, treeSha: '9f00aa',
  cwd: '/x/checkout', services: [], startedAt: 0, endedAt: 1, ...extra });
const web = (state, detail) => ({ service: 'web', state, detail, url: 'http://localhost:18431/healthz' });
const blockedE2e = (checkId) => check(checkId, 'e2e', 'blocked', {
  detail: 'blocked: environment not started — \`web\` is stopped', services: [web('stopped', null)] });
const startView = (flow, worktree, checks, roles) => ({ flow, binding: 'sha256:5a7e',
  request: { project: '/x/webapp', worktree: id(worktree),
    services: [{ service: 'web', label: 'web', argv: ['python3', 'server.py', '{{ env.WEB_PORT }}'] }],
    checks, roles } });
const e2eStart = [{ check: 'e2e', label: 'browser e2e', argv: ['npx', 'playwright', 'test'], needs: ['web'] }];
const done2 = [
  step(61, 'conventions', 'claude', 'done', { result: findings(2) }),
  step(62, 'correctness', 'codex', 'done', { result: findings(3) }),
];
const ended = [invocation(61, 61, 'conventions', 'submitted', { endedAt: 252000, instructionSources: ['REVIEW.md'] }),
  invocation(62, 62, 'correctness', 'submitted', { endedAt: 200000 })];
const runs = {
  [id('running')]: run(1, 'running', { phase: 'reviewing' }, [
      step(11, 'conventions', 'claude', 'done', { result: findings(2) }),
      step(12, 'correctness', 'codex', 'running'),
      step(13, 'ui', 'claude', 'skipped', { detail: 'none of its paths changed' }),
      step(14, 'security', 'codex', 'pending'),
    ], [invocation(1, 12, 'correctness', 'running', { progress: 'reading export.py', attempt: 2 })], [], true, {
      queued: [{ seat: { id: 7, flow: 1, step: 14, role: 'security', provider: 'codex', askedAt: 1, admittedAt: null },
        reason: { kind: 'provider', provider: 'codex', open: 3, cap: 3 }, position: 1, words: 'queued: Codex at 3/3' }] }),
  [id('waiting')]: run(2, 'waiting', { phase: 'reviewing', hold: { kind: 'waiting_capacity', provider: 'codex', resetsAt: 1792560000 } }, [
      step(21, 'conventions', 'claude', 'done', { result: findings(1) }),
      step(22, 'correctness', 'codex', 'waiting_capacity', { resetsAt: 1792560000, detail: "You've hit your usage limit." }),
    ], [], [], true, { capacity: { step: 22, role: 'correctness', provider: 'codex', model: null,
      resetsAt: 1792560000, fallbacks: [], allowCreditBilling: false, recheck: null } }),
  [id('interrupted')]: run(3, 'interrupted', { phase: 'reviewing', hold: { kind: 'interrupted', cause: 'quit' } }, [
      step(31, 'conventions', 'claude', 'done', { result: findings(0) }),
      step(32, 'correctness', 'codex', 'interrupted'),
    ]),
  [id('failed-role')]: run(4, 'failed-role', { phase: 'consolidated' }, [
      step(41, 'conventions', 'claude', 'done', { result: findings(3) }),
      step(42, 'ui', 'claude', 'failed', { failures: 2, detail: 'it had no result after 15 min' }),
    ]),
  [id('reviewed')]: run(5, 'reviewed', { phase: 'consolidated' }, [
      step(51, 'conventions', 'claude', 'done', { result: findings(0) }),
      step(52, 'correctness', 'codex', 'replaced', { detail: 'handed over to correctness-claude' }),
      step(53, 'correctness-claude', 'claude', 'done', { result: findings(1), standsInFor: 'correctness',
        handover: '\`correctness\` on Codex stopped before handing in a result, because its provider reached a usage limit.\\nDiff stat: 1 file, +2 −0' }),
    ], [], [], false, { switches: [{ id: 1, flow: 5, fromStep: 52, toStep: 53, reason: 'limit', by: 'config', at: 1,
      handover: { from: { role: 'correctness', provider: 'codex', model: null }, to: { role: 'correctness-claude', provider: 'claude', model: 'sonnet' },
        reason: 'limit', why: 'x', objective: 'x', stage: 0, attempts: 1, progress: [], plan: null, snapshot: null, diff: [], findings: [], answers: [] } }],
      findings: [record(50, 5, { kind: 'suggestion' }, 'Finding 0', 'x', at('src/a.rs', 3),
      [{ source: source('correctness-claude', 'claude', 53), reported: reported('Finding 0', 'suggestion') }])] }),
  // A review with a finding of every class, two roles agreeing on one, a dismissed one, and checks.
  [id('findings')]: run(6, 'findings', { phase: 'consolidated' }, done2, ended, [], false, {
    findings: [
      record(61, 6, { kind: 'confirmed_bug', verification: 'ran check query_count: 5,003 queries for 5,000 rows (evidence c3)' },
        'Each row loads unit.parent separately (N+1 queries)', 'The export loop reads \`unit.parent.name\` for every row.',
        at('exports/export.py', 88, 92), [
          { source: source('correctness', 'codex', 62), reported: reported('Each row loads unit.parent separately (N+1 queries)', 'confirmed_bug', { verification: 'ran check query_count', evidence: ['c3: 5,003 queries'] }) },
          { source: source('conventions', 'claude', 61), reported: reported('Parent lookups in a loop', 'suggestion') },
        ], { severity: 'blocking', requirement: 'AC 2: export 5,000 units in under 10 s' }),
      record(62, 6, { kind: 'rule_violation', rule: 14, revision: 2 }, 'Confirmation uses BaseModal instead of AppDialog',
        'AppDialog carries the focus trap and Escape handling this flow needs.', at('src/exports/ExportModal.vue', 12), [
          { source: source('conventions', 'claude', 61, 1), reported: reported('Confirmation uses BaseModal', 'rule_violation', { rule: { id: 14, revision: 2 } }) },
        ]),
      record(63, 6, { kind: 'conflict', between: ['R-21', 'REVIEW.md §Naming'] }, 'R-21 and REVIEW.md disagree about export file names',
        'R-21 says kebab-case; REVIEW.md says snake_case for downloads.', null, [
          { source: source('conventions', 'claude', 61, 2), reported: reported('R-21 and REVIEW.md disagree', 'conflict', { between: ['R-21', 'REVIEW.md §Naming'] }) },
        ]),
      record(64, 6, { kind: 'suggestion' }, 'Name the export job', 'A named job is easier to find in logs.', at('exports/jobs.py', 1), [
          { source: source('correctness', 'codex', 62, 1), reported: reported('Name the export job', 'suggestion') },
        ], { severity: 'minor', lines: false }),
      record(65, 6, { kind: 'suggestion' }, 'Prefer f-strings', 'Consistency.', at('exports/export.py', 3), [
          { source: source('correctness', 'codex', 62, 2), reported: reported('Prefer f-strings', 'suggestion') },
        ], { severity: 'minor', dismissed: { reason: 'the module predates f-strings on purpose', at: 1 } }),
    ],
    checks: [check(1, 'lint', 'passed'), check(2, 'unit', 'failed'), blockedE2e(3)],
    pendingStart: startView(6, 'findings', e2eStart, []),
    expectedChecks: [
      { id: 'lint', label: 'lint', needs: [], roles: ['correctness'] },
      { id: 'unit', label: 'unit tests', needs: [], roles: ['correctness'] },
      { id: 'e2e', label: 'browser e2e', needs: ['web'], roles: ['correctness'] },
      { id: 'build', label: 'build', needs: [], roles: ['correctness'] },
    ],
    applied: { rules: [{ rule: 14, revision: 2, kind: 'required', text: 'Use AppDialog for confirm and cancel flows', roles: ['conventions'] },
      { rule: 3, revision: 1, kind: 'guidance', text: 'Prefer querysets', roles: ['conventions'] }],
      sources: ['REVIEW.md'], skills: [{ role: 'correctness', provider: 'codex', skill: 'team-review' }] },
  }),
  [id('stale')]: run(7, 'stale', { phase: 'consolidated' }, done2, ended, [], false, {
    findings: [
      record(71, 7, { kind: 'suggestion' }, 'Export loop', 'x', at('export.py', 3), [{ source: source('correctness', 'codex', 62), reported: reported('Export loop', 'suggestion') }]),
      record(72, 7, { kind: 'suggestion' }, 'Untouched file', 'x', at('models.py', 9), [{ source: source('conventions', 'claude', 61), reported: reported('Untouched file', 'suggestion') }]),
    ] }),
  [id('clean')]: run(8, 'clean', { phase: 'consolidated' }, [
      step(81, 'conventions', 'claude', 'done', { result: findings(0) }),
      step(82, 'correctness', 'codex', 'done', { result: findings(0) }),
    ], ended, [], false, {
    checks: [check(4, 'lint', 'passed'), blockedE2e(5)],
    pendingStart: startView(8, 'clean', e2eStart, []),
    expectedChecks: [{ id: 'lint', label: 'lint', needs: [], roles: ['correctness'] }, { id: 'e2e', label: 'browser e2e', needs: ['web'], roles: ['correctness'] }],
    applied: { rules: [{ rule: 3, revision: 1, kind: 'guidance', text: 'Prefer querysets', roles: ['conventions'] }], sources: ['CLAUDE.md'], skills: [] },
  }),
  [id('skill')]: run(9, 'skill', { phase: 'consolidated' }, [
      step(91, 'conventions', 'claude', 'failed', { detail: 'Claude does not list the skill \`team-review\`',
        skillProblem: { kind: 'missing', role: 'conventions', provider: 'claude', skill: 'team-review' } }),
      step(92, 'correctness', 'codex', 'done', { result: findings(0) }),
    ]),
  // A runtime role waiting for its environment: the run is held, and nothing has run.
  [id('blocked-role')]: run(10, 'blocked-role', { phase: 'reviewing', hold: { kind: 'blocked', cause: 'env' } }, [
      step(101, 'conventions', 'claude', 'done', { result: findings(0) }),
      step(102, 'clickthrough', 'claude', 'pending', { stage: 1, workspace: 'worktree',
        detail: 'blocked: environment not started — \`web\` is stopped It waits for you to start the environment: Reviews › Start environment and run, or the inbox.' }),
    ], [], [], true, { pendingStart: startView(10, 'blocked-role', [], ['clickthrough']) }),
  // A runtime role's finding, with the screenshot it cites, and the check it ran with web up.
  [id('runtime')]: run(11, 'runtime', { phase: 'consolidated' }, [
      step(111, 'conventions', 'claude', 'done', { result: findings(0) }),
      step(112, 'clickthrough', 'claude', 'done', { stage: 1, workspace: 'worktree', result: findings(1) }),
    ], [invocation(112, 112, 'clickthrough', 'submitted', { endedAt: 90000 })], [], false, {
    findings: [record(111, 11, { kind: 'confirmed_bug', verification: 'clicked Save on / and nothing happened' },
      'Save does nothing', 'The Save button has no handler, so a click changes nothing.', null, [
        { source: { ...source('clickthrough', 'claude', 112), workspace: 'worktree' },
          reported: reported('Save does nothing', 'confirmed_bug', { verification: 'clicked Save on / and nothing happened', evidence: ['s2'] }) },
      ], { severity: 'blocking' })],
    checks: [check(6, 'e2e', 'passed', { role: 'clickthrough', cwd: id('runtime'),
      services: [web('healthy', 'answered 200 in 3 ms')] })],
    expectedChecks: [{ id: 'e2e', label: 'browser e2e', needs: ['web'], roles: ['clickthrough'] }],
    screenshots: [
      { id: 1, flow: 11, step: 112, invocation: 112, role: 'clickthrough', url: 'http://localhost:18431/', takenBy: 'wtm', takenAt: 1 },
      { id: 2, flow: 11, step: 112, invocation: 112, role: 'clickthrough', url: 'http://localhost:18431/', takenBy: 'role', takenAt: 2 },
    ],
  }),
};
// A review of your own work whose findings can be fixed: two in files, a suggestion, one about the
// change as a whole, and one dismissed. The writer roles are what the config has.
runs[id('fixable')] = run(12, 'fixable', { phase: 'consolidated' }, done2, ended, [], false, {
  writers: [
    { id: 'fixer', provider: 'claude', model: 'sonnet', objective: 'Fix the findings you are given' },
    { id: 'fixer-codex', provider: 'codex', model: null, objective: 'Fix the findings you are given' },
  ],
  findings: [
    record(121, 12, { kind: 'confirmed_bug', verification: 'ran the export with no rows' },
      'The export assumes it has rows', 'An empty export raises on the first row.',
      at('src/export.rs', 3), [{ source: source('correctness', 'codex', 62), reported: reported('The export assumes it has rows', 'confirmed_bug') }],
      { severity: 'blocking' }),
    record(122, 12, { kind: 'rule_violation', rule: 14, revision: 2 }, 'Confirmation uses BaseModal instead of AppDialog',
      'AppDialog carries the focus trap this flow needs.', at('src/ExportModal.vue', 12),
      [{ source: source('conventions', 'claude', 61), reported: reported('Confirmation uses BaseModal', 'rule_violation', { rule: { id: 14, revision: 2 } }) }]),
    record(123, 12, { kind: 'suggestion' }, 'Name the export job', 'A named job is easier to find in logs.',
      at('src/jobs.rs', 1), [{ source: source('correctness', 'codex', 62, 1), reported: reported('Name the export job', 'suggestion') }],
      { severity: 'minor' }),
    record(124, 12, { kind: 'conflict', between: ['R-21', 'REVIEW.md §Naming'] }, 'R-21 and REVIEW.md disagree about file names',
      'R-21 says kebab-case; REVIEW.md says snake_case.', null,
      [{ source: source('conventions', 'claude', 61, 2), reported: reported('R-21 and REVIEW.md disagree', 'conflict', { between: ['R-21', 'REVIEW.md §Naming'] }) }]),
    record(125, 12, { kind: 'suggestion' }, 'Prefer f-strings', 'Consistency.', at('src/old.py', 3),
      [{ source: source('correctness', 'codex', 62, 2), reported: reported('Prefer f-strings', 'suggestion') }],
      { severity: 'minor', dismissed: { reason: 'the module predates f-strings on purpose', at: 1 } }),
  ],
});
/** The fix the mocked writer makes, as the app records it, and the diff it reads by. */
const FIX_HEAD = 'f1a2b3c4d5e6f708090a0b0c0d0e0f1011121314';
const writerFix = (state, request) => ({ id: 1, flow: 12, step: 120, role: request.role, findings: request.findings,
  scope: { paths: ['src/ExportModal.vue', 'src/export.rs'] }, baseSha: 'b5d7e49c72700c346df25b998291aa4487d293f3',
  headSha: state === 'ready' ? FIX_HEAD : null, diffHash: state === 'ready' ? 'sha256:7c0d1e2f3a4b5c6d' : null,
  state, detail: null, asked: [], allowed: [], startedAt: 1, endedAt: state === 'writing' ? null : 2 });
window.refuseFix = null;
window.finishFix = () => {
  const r = runs[id('fixable')];
  r.flow.state = { ...r.flow.state, phase: 'fix_ready' };
  r.fixes = [writerFix('ready', { role: r.fixes[0].role, findings: r.fixes[0].findings })];
  r.leases = [];
  // This harness mocks no events, so the window is told what the flow:changed event would tell it.
  return window.harness.reviewRuns.load('/x/webapp', id('fixable'));
};
/** A real PNG, 16 by 10 and green, so the screenshot draws as an image rather than a broken one. */
const PNG = 'iVBORw0KGgoAAAANSUhEUgAAABAAAAAKCAIAAAAy3EnLAAAAE0lEQVR4nGPQ6w4nCTGMahiaGgCjyKoB4OYs+gAAAABJRU5ErkJggg==';
const staleness = {
  6: { flow: 6, treeSha: '9f00aa', headSha: 'a1b2c3d4e5f6', changed: false, paths: [], complete: true, staleFindings: [], staleChecks: [], affected: [] },
  7: { flow: 7, treeSha: '4e5f6a', headSha: '4e5f6a7b8c', changed: true, paths: ['export.py', 'ExportModal.vue'], complete: true, staleFindings: [71], staleChecks: [], affected: ['correctness', 'conventions'] },
  8: { flow: 8, treeSha: '9f00aa', headSha: 'a1b2c3d4e5f6', changed: false, paths: [], complete: true, staleFindings: [], staleChecks: [], affected: [] },
};
const item = (n, flow, kind, target, worktree, title, body) => ({ id: n, flow, kind, target, title, body,
  state: 'open', createdAt: 0, closedAt: null, project: '/x/webapp', worktree: id(worktree), question: null });
const inbox = [
  item(1, 2, 'capacity', { kind: 'step', step: 22, role: 'correctness' }, 'waiting', '\`correctness\` is paused: Codex limit reached', "You've hit your usage limit."),
  item(2, 4, 'failure', { kind: 'step', step: 42, role: 'ui' }, 'failed-role', '\`ui\` failed: it had no result after 15 min', 'It had no result after 15 min after 2 attempts.'),
];
window.calls = [];
mockIPC((cmd, args) => {
  const a = args ?? {};
  window.calls.push({ cmd, args: a });
  switch (cmd) {
    case 'get_pref': return localStorage.getItem(prefKey(a.key));
    case 'set_pref': localStorage.setItem(prefKey(a.key), a.value); return null;
    case 'list_projects': return [structuredClone(project)];
    case 'list_worktrees': return structuredClone(worktrees);
    case 'sidebar_layout': return { groups: [], origins: {} };
    case 'list_agents': return [];
    case 'list_openers': return { openers: [], preferred: null };
    case 'usage_limits': return [];
    case 'browser_available': return { runtime: false, reason: 'harness' };
    case 'notification_permission': return 'granted';
    case 'dictation_status': return { available: false, recording: false, key: null, reason: 'harness' };
    case 'doctor': return { path: [], tools: [], problems: [] };
    case 'review_recipes': return [
      { id: 'standard', label: 'Standard', roles: [{ id: 'conventions', provider: 'claude', model: 'sonnet' }, { id: 'correctness', provider: 'codex', model: null }] },
      { id: 'quick', label: 'Quick', roles: [{ id: 'correctness', provider: 'codex', model: null }] },
    ];
    case 'latest_review': return structuredClone(runs[a.worktree] ?? null);
    case 'review_run': return structuredClone(Object.values(runs).find((r) => r.flow.id === a.flow));
    case 'review_staleness':
      if (!staleness[a.flow]) throw { kind: 'flow', message: 'no snapshot', detail: null };
      return structuredClone(staleness[a.flow]);
    case 'rerun_review': {
      const r = Object.values(runs).find((x) => x.flow.id === a.flow);
      return structuredClone({ ...r, running: true, flow: { ...r.flow, state: { ...r.flow.state, phase: 'reviewing' } } });
    }
    case 'dismiss_finding': {
      for (const r of Object.values(runs)) {
        const f = r.findings.find((x) => x.id === a.finding);
        if (f) { f.dismissed = a.reason === null ? null : { reason: a.reason, at: 2 }; return structuredClone(f); }
      }
      throw { kind: 'flow', message: 'no finding', detail: null };
    }
    case 'start_review':
      if (a.request.worktree === id('billing'))
        throw { kind: 'billing', message: "Workflow runs won't start until Claude Code is signed in with a claude.ai subscription.", detail: { providers: ['claude'] } };
      return structuredClone({ ...runs[id('running')], review: { ...runs[id('running')].review, worktree: a.request.worktree, recipe: a.request.recipe } });
    case 'review_screenshot': return PNG;
    case 'start_local_fix': {
      if (window.refuseFix) throw { kind: 'refused', message: window.refuseFix, detail: null };
      const r = Object.values(runs).find((x) => x.flow.id === a.flow);
      r.flow.state = { ...r.flow.state, phase: 'fixing' };
      r.fixes = [writerFix('writing', a.request)];
      r.leases = [{ id: 1, worktree: r.review.worktreePath, flow: a.flow, kind: 'writer', holder: a.request.role,
        acquiredAt: 1, releasedAt: null, releasedWhy: null }];
      return structuredClone(r);
    }
    case 'local_fix_diff': return {
      fix: runs[id('fixable')].fixes[0], base: 'b5d7e49c72700c346df25b998291aa4487d293f3', head: FIX_HEAD,
      hash: 'sha256:7c0d1e2f3a4b5c6d',
      files: [{ path: 'src/ExportModal.vue', added: 4, removed: 2 }, { path: 'src/export.rs', added: 3, removed: 1 }],
      patch: 'diff --git a/src/export.rs b/src/export.rs\\n--- a/src/export.rs\\n+++ b/src/export.rs\\n@@ -1,1 +1,3 @@\\n-fn export() { for row in rows() { write(row[0]) } }\\n+fn export() {\\n+    for row in rows() { write(row.first()) }\\n+}\\n',
      truncated: false, uncommitted: [], outliers: [] };
    // Settings › Recipes, which panel 9's Edit recipe and Move role open on the role.
    case 'recipe_settings': return {
      file: { home: 'local', declaring: ['local'], roleHomes: {}, recipeHomes: {}, userPath: '/u/config.toml',
        localPath: '/x/webapp/.git/wtm.local.toml', rootKey: '/x/webapp', revision: '1' },
      places: [{ home: 'local', header: '[workflow]', path: '/x/webapp/.git/wtm.local.toml' }],
      workflow: { planner: null, designer: null, implement: null, checks: [],
        role: { conventions: { objective: 'Conventions', instructions: null, provider: 'claude', model: 'sonnet',
          effort: 'high', skills: ['team-review'], inputs: ['diff'], output: 'findings', profile: 'reviewer',
          workspace: null, timeout_ms: null, retries: 1, fallback: 'ask' } },
        recipe: { standard: { label: 'Standard', stage: [{ roles: ['conventions'] }], concurrency: null } } } };
    case 'check_recipes': return null;
    case 'start_review_environment': {
      const r = Object.values(runs).find((x) => x.flow.id === a.flow);
      return structuredClone({ ...r, pendingStart: null, running: true,
        flow: { ...r.flow, state: { ...r.flow.state, hold: null } } });
    }
    case 'choose_capacity': {
      const r = Object.values(runs).find((x) => x.flow.id === a.flow);
      if (a.choice.kind === 'wait') return structuredClone(r);
      return structuredClone({ ...r, capacity: null, running: true,
        flow: { ...r.flow, state: { ...r.flow.state, hold: null } } });
    }
    case 'run_sessions': return { seq: 1, caps: { total: 6, claude: 3, codex: 3 }, holding: [], queued: [], labels: {} };
    case 'resume_review': return structuredClone({ ...Object.values(runs).find((r) => r.flow.id === a.flow), running: true, flow: { ...Object.values(runs).find((r) => r.flow.id === a.flow).flow, state: { kind: 'review_mine', end: null, hold: null, phase: 'reviewing' } } });
    // Open in Code lands on the Code tab, which reads the file: a small one is enough to draw.
    case 'code_tree': return { files: ['src/exports/ExportModal.vue'], ignored: [], dirs: [], symlinks: [], missing: [], truncated: false };
    case 'code_read_file': return { text: '<template>\\n</template>\\n', size: 22, truncated: false, mtimeMs: 1, symlink: false, outside: false };
    case 'code_changes': return { scope: 'uncommitted', against: 'HEAD', rev: 'HEAD', changes: [] };
    case 'work_inbox': return structuredClone(inbox);
    case 'resolve_inbox_item':
    case 'dismiss_inbox_item': {
      const at = inbox.findIndex((x) => x.id === a.item);
      if (at < 0) return null;
      const [closed] = inbox.splice(at, 1);
      return structuredClone({ ...closed, state: cmd === 'dismiss_inbox_item' ? 'dismissed' : 'resolved', closedAt: 1 });
    }
    default: return cmd.startsWith('list_') ? [] : null;
  }
});
const { mount } = await import('svelte');
await import('/src/styles/main.scss');
const { default: App } = await import('/src/App.svelte');
const { workspace } = await import('/src/lib/state/workspace.svelte.ts');
const { work } = await import('/src/lib/state/work.svelte.ts');
const { view } = await import('/src/lib/state/view.svelte.ts');
const { codeRequests } = await import('/src/lib/state/code-request.svelte.ts');
const { reviewRuns } = await import('/src/lib/state/review-runs.svelte.ts');
window.harness = { emit, workspace, work, view, codeRequests, reviewRuns };
mount(App, { target: document.getElementById('app') });`;

const server = await createServer({
    configFile: false,
    envDir: false,
    logLevel: 'warn',
    plugins: [
        svelte(),
        {
            name: 'review-run-probe',
            resolveId: (id) => (id === '/probe-entry.js' ? '\0probe-entry' : undefined),
            load: (id) => (id === '\0probe-entry' ? entry : undefined),
            configureServer(dev) {
                dev.middlewares.use((request, response, next) => {
                    if (request.url === '/favicon.ico') {
                        response.statusCode = 204;
                        response.end();
                        return;
                    }
                    if (request.url?.split('?')[0] !== '/probe') return next();
                    response.setHeader('Content-Type', 'text/html');
                    response.end(
                        '<!doctype html><html lang="en"><body><div id="app"></div>' +
                            '<script type="module" src="/probe-entry.js"></script></body></html>',
                    );
                });
            },
        },
    ],
    server: { host: '127.0.0.1', port: 0 },
});

let browser;
try {
    await server.listen();
    const base = `http://127.0.0.1:${server.httpServer.address().port}/probe`;
    browser = await chromium.launch({
        executablePath:
            process.env.WTM_CHROME ||
            '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
        headless: true,
    });

    async function open(scheme) {
        const context = await browser.newContext({
            viewport: { width: 1280, height: 800 },
            colorScheme: scheme,
        });
        const page = await context.newPage();
        page.on('pageerror', (e) => {
            throw new Error(`page error: ${e.message}`);
        });
        page.on('console', (message) => {
            if (message.type() === 'error') console.error('CONSOLE', message.text());
        });
        await page.goto(`${base}?theme=${scheme}`);
        await page.waitForFunction(() => window.harness?.workspace.worktrees.length > 0);
        await page.getByRole('region', { name: 'Work', exact: true }).waitFor();
        return page;
    }

    const shot = (page, path) => page.screenshot({ path, animations: 'disabled' });
    const calls = (page, cmd) =>
        page.evaluate((c) => window.calls.filter((x) => x.cmd === c), cmd);
    const panel = (page) => page.locator('#work-panel');
    /** Pick a worktree's row the way the queue does, by id: the row labels are R1's to word. */
    async function pick(page, dir) {
        const target = dir === 'webapp' ? '/x/webapp' : `/x/webapp-wt/${dir}`;
        await page.evaluate((id) => window.harness.work.pick(id), target);
        await page.waitForFunction(
            (id) =>
                document
                    .querySelector('.c-work__meta')
                    ?.textContent?.includes(
                        id === '/x/webapp' ? 'main' : id.split('/').at(-1),
                    ),
            target,
        );
    }
    async function readout(page, eyebrow, title) {
        await panel(page).locator('.c-work-state__eyebrow', { hasText: eyebrow }).waitFor();
        const text = await panel(page).textContent();
        assert(text.includes(title), `expected "${title}" in: ${text}`);
        return text;
    }

    for (const scheme of ['light', 'dark']) {
        const page = await open(scheme);

        // ── The inbox: the project's open run items, newest first ─────────────────────────────
        const inboxText = await page
            .getByRole('complementary', { name: /Inbox/ })
            .textContent();
        assert(inboxText.includes('Inbox 2'), inboxText);
        assert(inboxText.includes('correctness is paused: Codex limit reached'), inboxText);
        assert.equal(
            await page.locator('.c-run-inbox__title code').first().textContent(),
            'correctness',
            'a backticked name is drawn as code, not with its backticks',
        );
        assert(inboxText.includes('Failed'));

        // ── Not reviewed, with two recipes to pick from ───────────────────────────────────────
        await pick(page, 'webapp');
        await readout(page, 'Not reviewed yet', 'Review main');
        const select = panel(page).getByRole('combobox', { name: 'Recipe' });
        assert.equal(await select.inputValue(), 'standard');
        assert((await panel(page).textContent()).includes('conventions on Claude sonnet'));
        await shot(page, join(shots, `not-reviewed-${scheme}.png`));

        // ── Running: one role done, one on its second attempt, one skipped ───────────────────
        await pick(page, 'running');
        let text = await readout(
            page,
            'Running',
            'Reviewing snapshot a1b2c3d + uncommitted',
        );
        assert(text.includes('done · 2 findings'), text);
        assert(text.includes('reading export.py · attempt 2'), text);
        assert(text.includes('Skipped, because none of their paths changed: ui.'), text);
        assert(text.includes('queued: Codex at 3/3 · next in line'), text);
        assert(text.includes('Waiting for a run-session place under your caps'), text);
        assert(await panel(page).getByRole('button', { name: 'Stop review' }).isVisible());
        assert.equal(
            await page.locator('.c-work__header .c-badge').textContent(),
            'Run: running',
            'the header says the run’s state, not “Not reviewed”',
        );
        await shot(page, join(shots, `running-${scheme}.png`));

        // ── Waiting for usage capacity ───────────────────────────────────────────────────────
        await pick(page, 'waiting');
        text = await readout(
            page,
            'Waiting for usage capacity',
            'correctness is paused: Codex limit reached',
        );
        assert(await panel(page).getByRole('button', { name: 'Try now' }).isVisible());
        assert(text.includes('never uses an API key'), text);
        assert(text.includes('paused for usage capacity'), text);
        assert(
            text.includes('resumes automatically if wtm is open and the Mac is awake'),
            text,
        );
        assert(text.includes('None configured for this role'), text);
        assert(text.includes('unknown: Codex has not reported figures'), text);
        await shot(page, join(shots, `waiting-capacity-${scheme}.png`));
        const capacity = panel(page).locator('.c-capacity');
        await capacity.getByRole('button', { name: 'Wait and resume' }).click();
        await capacity
            .getByRole('button', { name: 'Run this role on Claude once…' })
            .click();
        await capacity.getByRole('combobox', { name: 'Model' }).waitFor();
        await shot(page, join(shots, `run-once-${scheme}.png`));
        await capacity.getByRole('button', { name: 'Run once on Claude' }).click();
        assert.deepEqual(
            (await calls(page, 'choose_capacity')).map((c) => c.args),
            [
                { flow: 2, step: 22, choice: { kind: 'wait' } },
                {
                    flow: 2,
                    step: 22,
                    choice: { kind: 'run_once', provider: 'claude', model: null },
                },
            ],
        );
        // The mock answers as if the role started on Claude; the run itself is unchanged, so
        // reading it again puts the waiting state back for the queue row below.
        await pick(page, 'running');
        await pick(page, 'waiting');
        await readout(page, 'Waiting for usage capacity', 'correctness is paused');

        // ── Interrupted by a quit: Resume asks the engine, nothing else does ────────────────
        await pick(page, 'interrupted');
        text = await readout(page, 'Interrupted', 'wtm quit during this review');
        assert(text.includes('1 of 2 roles had finished'), text);
        assert(text.includes('nothing was sent again on relaunch'), text);
        await shot(page, join(shots, `interrupted-${scheme}.png`));
        await panel(page).getByRole('button', { name: 'Resume' }).click();
        assert.deepEqual(
            (await calls(page, 'resume_review')).map((c) => c.args),
            [{ flow: 3 }],
        );
        await readout(page, 'Running', 'Reviewing snapshot');

        // ── A role failed: the review is incomplete until it is retried ─────────────────────
        await pick(page, 'failed-role');
        text = await readout(page, 'A role failed', 'ui did not finish');
        assert(text.includes('failed: it had no result after 15 min'), text);
        assert(
            await panel(page)
                .getByRole('button', { name: 'Retry failed roles' })
                .isVisible(),
        );
        await shot(page, join(shots, `role-failed-${scheme}.png`));

        // ── Reviewed, with a fallback that stood in for a role ──────────────────────────────
        await pick(page, 'reviewed');
        text = await readout(page, 'Reviewed', '1 finding from 2 roles');
        assert(text.includes('correctness-claude (for correctness)'), text);
        assert(text.includes('handed over'), text);
        const handover = panel(page).locator('.c-handover');
        assert(
            (await handover.locator('summary').textContent()).includes(
                'because its provider reached a usage limit · by its fallback chain in your config',
            ),
        );
        await handover.locator('summary').click();
        assert((await handover.locator('pre').textContent()).includes('Diff stat: 1 file'));
        await shot(page, join(shots, `reviewed-${scheme}.png`));

        // ── A review with findings of every class ───────────────────────────────────────────
        await pick(page, 'findings');
        text = await readout(page, 'Reviewed', '4 findings from 2 roles');
        assert(text.includes('lint passed. unit tests failed (exit 1).'), text);
        assert(text.includes('build was not run, so this is not a runtime pass.'), text);
        const queueText = await page.locator('.c-work-queue').textContent();
        assert(
            queueText.includes('Paused for usage capacity'),
            'a held run is said to be paused, not working',
        );
        const meta = await panel(page).locator('.c-review__meta').textContent();
        assert(meta.includes('Snapshot a1b2c3d') && meta.includes('origin/main'), meta);
        assert(meta.includes('4m 12s'), meta);
        const headings = await panel(page)
            .locator('.c-findings__heading')
            .allTextContents();
        assert.deepEqual(
            headings.map((h) => h.trim()),
            [
                'Confirmed bugs · 1',
                'Rule violations · 1',
                'Conflicts to resolve · 1',
                'Suggestions · 1',
                'Dismissed · 1',
                'Checks · run by wtm, never on an agent’s say-so'.replace('’', "'"),
            ],
        );
        const bug = panel(page).locator('.c-finding', { hasText: 'N+1 queries' });
        const bugText = await bug.textContent();
        assert(
            bugText.includes('Blocking') && bugText.includes('exports/export.py:88–92'),
            bugText,
        );
        assert(
            bugText.includes('correctness · Codex gpt-6 · conventions · Claude sonnet'),
            bugText,
        );
        assert(bugText.includes('Verified: ran check query_count'), bugText);
        assert(bugText.includes('Requirement: AC 2'), bugText);
        assert(
            bugText.includes('Also reported by one more report'),
            'the second role’s report is kept',
        );
        const rule = await panel(page)
            .locator('.c-finding', { hasText: 'BaseModal' })
            .textContent();
        assert(
            rule.includes('Required rule') &&
                rule.includes('Rule R-14 v2 (required): "Use AppDialog'),
            rule,
        );
        const conflict = await panel(page)
            .locator('.c-finding', { hasText: 'disagree' })
            .textContent();
        assert(
            conflict.includes('About the change as a whole') &&
                conflict.includes('did not pick one'),
            conflict,
        );
        assert.equal(
            await panel(page).locator('details.c-findings').first().getAttribute('open'),
            null,
            'suggestions start folded',
        );
        const checksText = await panel(page).locator('.c-review-checks').textContent();
        for (const line of [
            'lint · passed',
            'unit tests · failed: exit 1',
            'browser e2e · blocked: environment not started — web is stopped',
            'build · not run',
        ])
            assert(checksText.includes(line), `${line} in ${checksText}`);
        assert(
            !/e2e · passed/.test(checksText),
            'a blocked check is never drawn as passed',
        );
        const applied = await panel(page).locator('.c-review__applied').textContent();
        assert(
            applied.includes('rules R-14 v2, R-3 v1') &&
                applied.includes('instruction sources REVIEW.md'),
            applied,
        );
        assert(
            applied.includes('skill /team-review (Codex)') &&
                applied.includes('read-only in a snapshot copy'),
            applied,
        );
        await shot(page, join(shots, `findings-${scheme}.png`));

        // Dismiss asks why, and sends the reason.
        await bug.getByRole('button', { name: 'Dismiss…' }).click();
        await bug
            .getByRole('textbox', { name: 'Why this finding is dismissed' })
            .fill('a known trade-off');
        await bug.getByRole('button', { name: 'Dismiss', exact: true }).click();
        await page.waitForFunction(() =>
            window.calls.some((c) => c.cmd === 'dismiss_finding'),
        );
        assert.deepEqual((await calls(page, 'dismiss_finding')).at(-1).args, {
            finding: 61,
            reason: 'a known trade-off',
        });
        await panel(page)
            .locator('.c-findings__heading', { hasText: 'Dismissed · 2' })
            .waitFor();

        // Save as rule opens R7's dialog on the stored finding.
        await panel(page)
            .locator('.c-finding', { hasText: 'BaseModal' })
            .getByRole('button', { name: 'Save as rule…' })
            .click();
        const dialog = page.getByRole('dialog', { name: 'Save as review rule' });
        await dialog.waitFor();
        assert(
            (await dialog.textContent()).includes('src/exports/ExportModal.vue'),
            'the dialog names the finding’s file',
        );
        await shot(page, join(shots, `save-as-rule-${scheme}.png`));
        await dialog.getByRole('button', { name: 'Cancel' }).click();
        await dialog.waitFor({ state: 'detached' });

        // Open in Code asks for the file at its line.
        await panel(page)
            .locator('.c-finding', { hasText: 'BaseModal' })
            .getByRole('button', { name: 'Open in Code' })
            .click();
        const request = await page.evaluate(() => window.harness.codeRequests.request);
        assert.deepEqual(
            { path: request.path, line: request.line, worktreeId: request.worktreeId },
            {
                path: 'src/exports/ExportModal.vue',
                line: 12,
                worktreeId: '/x/webapp-wt/findings',
            },
        );
        await page.evaluate(() => window.harness.view.show('work'));
        await page.getByRole('region', { name: 'Work', exact: true }).waitFor();

        // ── Stale: a new commit touched two files; Rerun affected names the roles ──────────
        await pick(page, 'stale');
        text = await readout(page, 'Stale', '2 files changed after this review');
        assert(
            text.includes('New commit 4e5f6a7 touched export.py, ExportModal.vue.'),
            text,
        );
        assert(
            text.includes(
                '1 finding and the overall result are stale. 1 finding on untouched files still holds.',
            ),
            text,
        );
        const staleCard = await panel(page)
            .locator('.c-finding', { hasText: 'Export loop' })
            .textContent();
        assert(staleCard.includes('Stale'), staleCard);
        assert(
            !(
                await panel(page)
                    .locator('.c-finding', { hasText: 'Untouched file' })
                    .textContent()
            ).includes('Stale'),
        );
        await page.waitForFunction(() =>
            document
                .querySelector('.c-work-queue')
                ?.textContent?.includes('2 findings · review stale'),
        );
        await shot(page, join(shots, `stale-${scheme}.png`));
        await panel(page).getByRole('button', { name: 'Rerun affected roles (2)' }).click();
        assert.deepEqual((await calls(page, 'rerun_review')).at(-1).args, {
            flow: 7,
            scope: 'affected',
        });
        await readout(page, 'Running', 'Reviewing snapshot');

        // ── Clean, with a check blocked on its environment: not a runtime pass ──────────────
        await pick(page, 'clean');
        text = await readout(page, 'Clean', 'No findings');
        assert(
            text.includes(
                '2 roles, rules R-3 v1 applied. lint passed. browser e2e was blocked (environment not started — web is stopped)',
            ),
            text,
        );
        assert(text.includes('so this is not a runtime pass'), text);
        const card = panel(page).locator('.c-start-approval');
        const cardText = await card.textContent();
        assert(
            cardText.includes('Approval needed · start the environment') &&
                cardText.includes(
                    'Blocked: environment not started. Check browser e2e needs',
                ) &&
                cardText.includes('Start web and run what it blocks?') &&
                cardText.includes('python3 server.py {{ env.WEB_PORT }}') &&
                cardText.includes('npx playwright test'),
            cardText,
        );
        await shot(page, join(shots, `clean-blocked-${scheme}.png`));
        await card.getByRole('button', { name: 'Start environment and run' }).click();
        assert.deepEqual((await calls(page, 'start_review_environment')).at(-1).args, {
            flow: 8,
            binding: 'sha256:5a7e',
        });

        // ── A runtime role waiting for its environment ───────────────────────────────────────
        await pick(page, 'blocked-role');
        text = await readout(
            page,
            'Blocked: environment not started',
            'clickthrough waits for web',
        );
        assert(text.includes('nothing has run, and nothing reads as passed'), text);
        assert(
            (await panel(page).locator('.c-start-approval').textContent()).includes(
                'Then lets run',
            ),
            'the card names the runtime role it lets run',
        );
        await shot(page, join(shots, `runtime-blocked-${scheme}.png`));

        // ── A runtime role's finding, with the screenshot it cites ──────────────────────────
        await pick(page, 'runtime');
        await readout(page, 'Reviewed', '1 finding from 2 roles');
        const seen = panel(page).locator('.c-finding', { hasText: 'Save does nothing' });
        await seen
            .getByRole('img', { name: 'Screenshot s2 of http://localhost:18431/' })
            .waitFor();
        assert(
            (await seen.textContent()).includes('taken by clickthrough'),
            'the picture says who took it',
        );
        const runtime = panel(page).locator('.c-review-runtime');
        assert.equal(await runtime.locator('.c-shot').count(), 2);
        assert(
            (await runtime.textContent()).includes('taken by wtm as the page first loaded'),
        );
        assert(
            (await panel(page).locator('.c-review-checks').textContent()).includes(
                'browser e2e · passed, with web at http://localhost:18431/healthz answering',
            ),
        );
        assert.deepEqual(
            (await calls(page, 'review_screenshot')).map((c) => c.args.id).sort(),
            [1, 2],
            'each screenshot is read once',
        );
        await shot(page, join(shots, `runtime-finding-${scheme}.png`));

        // ── Can't run: a role names a skill its provider does not list ──────────────────────
        await pick(page, 'skill');
        text = await readout(
            page,
            "Can't run: recipe",
            "/team-review isn't available to Claude here",
        );
        assert(text.includes('Providers don’t share skills'.replace('’', "'")), text);
        assert(text.includes('a review keeps the recipe it started with'), text);
        await shot(page, join(shots, `missing-skill-${scheme}.png`));
        // Panel 9: Move role to Codex opens the role's editor in Settings › Recipes, Codex chosen,
        // to be checked by Apply and written by Save: the click itself changes nothing.
        assert(await panel(page).getByRole('button', { name: 'Edit recipe' }).isVisible());
        await panel(page).getByRole('button', { name: 'Move role to Codex' }).click();
        const moving = page.getByRole('dialog', { name: 'Edit role conventions' });
        await moving.waitFor();
        assert.equal(await moving.getByLabel('Provider').inputValue(), 'codex');
        assert.equal(await moving.getByLabel('Model').inputValue(), '');
        assert.equal(
            await page
                .getByRole('dialog', { name: 'Settings' })
                .getByRole('tab', { name: 'Recipes' })
                .getAttribute('aria-selected'),
            'true',
        );
        assert.equal(
            (await calls(page, 'save_recipes')).length,
            0,
            'nothing is saved by the click',
        );
        await moving.getByRole('button', { name: 'Cancel' }).click();
        await page
            .getByRole('dialog', { name: 'Settings' })
            .getByRole('button', { name: 'Done' })
            .click();

        // ── Can't start: billing — nothing was recorded, so it is the refusal that is drawn ──
        await pick(page, 'billing');
        await readout(page, 'Not reviewed yet', 'Review fix/billing');
        await panel(page).getByRole('button', { name: 'Run Review' }).click();
        text = await readout(
            page,
            "Can't start: billing",
            'Claude would not bill your subscription',
        );
        assert(
            text.includes('claude /login') && text.includes('nothing was started'),
            text,
        );
        assert(await panel(page).getByRole('button', { name: 'Check again' }).isVisible());
        await shot(page, join(shots, `billing-${scheme}.png`));

        // ── Your own work: Fix findings ──────────────────────────────────────────────────────
        await pick(page, 'fixable');
        const fixButton = panel(page).getByRole('button', { name: 'Fix findings…' });
        await fixButton.waitFor();
        assert.equal(
            await panel(page).getByRole('button', { name: /Finish review|choose where it goes/ }).count(),
            0,
            'your own work has no outcome sheet: that is a teammate’s pull request',
        );
        await fixButton.click();
        const sheet = page.getByRole('dialog', { name: 'Fix findings' });
        await sheet.waitFor();
        const picks = sheet.locator('.c-fix-sheet__pick');
        assert.equal(await picks.count(), 3, 'a file-bearing finding each: not the whole-change one, not the dismissed');
        assert.deepEqual(
            await picks.evaluateAll((rows) => rows.map((row) => row.querySelector('input').checked)),
            [true, true, false],
            'a bug and a rule violation are picked; a suggestion is not, as for a published review',
        );
        let sheetText = await sheet.innerText();
        assert(sheetText.includes('src/export.rs:3'), sheetText);
        assert(sheetText.includes('Name the export job'), sheetText);
        assert(!sheetText.includes('f-strings'), 'a dismissed finding is not offered');
        assert(sheetText.includes('name no file to hold a writer to'), sheetText);
        const writerSelect = sheet.getByRole('combobox', { name: 'Writer' });
        assert.equal(await writerSelect.inputValue(), 'fixer');
        assert.deepEqual(
            await writerSelect.locator('option').allTextContents(),
            ['fixer · claude sonnet', 'fixer-codex · codex'],
        );
        await shot(page, join(shots, `fix-pick-${scheme}.png`));
        const startFix = sheet.getByRole('button', { name: 'Start the fix' });
        for (const box of await picks.locator('input').all()) await box.uncheck();
        assert(await startFix.isDisabled(), 'nothing picked, nothing to start');
        for (const box of await picks.locator('input').all()) await box.check();

        // The app's refusal is shown where the click was, and nothing changed.
        await page.evaluate(() => {
            window.refuseFix =
                'The review worktree has changes nobody committed (src/draft.rs). A fix starts from a clean tree.';
        });
        await writerSelect.selectOption('fixer-codex');
        await startFix.click();
        await sheet.getByRole('alert').waitFor();
        assert((await sheet.getByRole('alert').innerText()).includes('changes nobody committed'));
        assert(await sheet.isVisible(), 'the sheet stays up so nothing picked is lost');
        assert.equal(await panel(page).getByRole('button', { name: 'Stop fix' }).count(), 0);
        await page.evaluate(() => (window.refuseFix = null));
        await writerSelect.selectOption('fixer');

        // Start sends exactly the picks and the writer, and closes the sheet.
        await picks.nth(2).locator('input').check();
        await startFix.click();
        await sheet.waitFor({ state: 'detached' });
        assert.deepEqual((await calls(page, 'start_local_fix')).at(-1).args, {
            flow: 12,
            request: { findings: [121, 122, 123], role: 'fixer' },
        });
        text = await readout(page, 'Fixing', 'fixer is fixing 3 findings');
        assert(!text.includes(' of #'), 'there is no pull request to name');
        assert(text.includes('wtm commits for it on your branch'), text);
        assert(text.includes('holds the review worktree as its writer'), text);
        assert(await panel(page).getByRole('button', { name: 'Stop fix' }).isVisible());
        assert.equal(
            await panel(page).getByRole('button', { name: 'Fix findings…' }).count(),
            0,
            'the writer has the worktree: a second fix cannot start',
        );
        await shot(page, join(shots, `fix-writing-${scheme}.png`));

        // Ready: it is on your branch, so what is offered is reading it and reviewing it again.
        await page.evaluate(() => window.finishFix());
        text = await readout(page, 'Fix ready', 'Fix of 3 findings committed as f1a2b3c');
        assert(text.includes('It is on your branch and nothing has left this Mac'), text);
        for (const name of ['Read what changed…', 'Fix findings…', 'Review the fix'])
            assert(await panel(page).getByRole('button', { name }).isVisible(), name);
        assert.equal(await panel(page).getByRole('button', { name: 'Stop fix' }).count(), 0);
        assert.equal(
            await panel(page).getByRole('button', { name: /Finish review|choose where it goes/ }).count(),
            0,
        );
        await shot(page, join(shots, `fix-ready-${scheme}.png`));

        // Reading it shows the files and the patch, and Review the fix starts a review.
        await panel(page).getByRole('button', { name: 'Read what changed…' }).click();
        const diff = page.getByRole('dialog', { name: 'What the fix changed' });
        await diff.locator('.c-local-fix__file').first().waitFor();
        sheetText = await diff.innerText();
        assert(sheetText.includes('src/export.rs') && sheetText.includes('+3'), sheetText);
        assert(sheetText.includes('fixer fixed 3 findings as f1a2b3c on b5d7e49'), sheetText);
        assert(sheetText.includes('+    for row in rows() { write(row.first()) }'), sheetText);
        assert.deepEqual((await calls(page, 'local_fix_diff')).at(-1).args, { flow: 12 });
        await shot(page, join(shots, `fix-read-${scheme}.png`));
        await diff.getByRole('button', { name: 'Review the fix' }).click();
        await diff.waitFor({ state: 'detached' });
        assert.deepEqual((await calls(page, 'start_review')).at(-1).args, {
            request: { project: '/x/webapp', worktree: '/x/webapp-wt/fixable', recipe: 'standard' },
        });

        // ── Run Review sends the recipe picked ───────────────────────────────────────────────
        await pick(page, 'webapp');
        await panel(page).getByRole('combobox', { name: 'Recipe' }).selectOption('quick');
        await panel(page).getByRole('button', { name: 'Run Review' }).click();
        assert.deepEqual((await calls(page, 'start_review')).at(-1).args, {
            request: { project: '/x/webapp', worktree: '/x/webapp', recipe: 'quick' },
        });

        // ── Dismissing an inbox item takes it away ───────────────────────────────────────────
        await page
            .getByRole('complementary', { name: /Inbox/ })
            .getByRole('button', { name: 'Dismiss' })
            .first()
            .click();
        await page.waitForFunction(() =>
            document.querySelector('.c-work__inbox')?.textContent?.includes('Inbox 1'),
        );
        assert.deepEqual((await calls(page, 'dismiss_inbox_item')).at(-1).args, {
            item: 1,
        });
        await page.context().close();
    }

    console.log(`ok — screenshots in ${shots}`);
} finally {
    await browser?.close();
    await server.close();
}

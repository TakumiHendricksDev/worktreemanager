/**
 * Drive the real app shell with mocked IPC: an issue's Implement tab and the inbox cards about its
 * implementation. No Tauri, no CLIs, no `acli`.
 *
 * What it proves, in order: a ready-to-test item leads to the Implement tab rather than offering to
 * pause the run; the tab says why the loop stopped (at its bound) and lists what it left with why,
 * each round with the commits the app made in it, and each configured check by its newest record —
 * a blocked one as blocked, never as passed. Then an implementation a quit cut off: its inbox card
 * and the tab both offer Resume, which asks Rust once. Then a question about a file outside the plan,
 * shown as waiting in the Inbox; and a project that names no implementer, which says so and offers
 * nothing to start.
 *
 * The mocked backend keeps what Rust would: the rounds, commits and checks, and whether Resume or
 * Start implementing is allowed.
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
const shots = process.env.WTM_SHOTS || join(tmpdir(), 'wtm-implement');
mkdirSync(shots, { recursive: true });

const entry = `
import { emit } from '@tauri-apps/api/event';
import { mockIPC, mockWindows } from '@tauri-apps/api/mocks';
const params = new URLSearchParams(location.search);
const scenario = params.get('scenario') ?? 'ready';
const prefKey = (k) => 'harness.pref.' + k;
localStorage.setItem(prefKey('ui.notify'), 'off');
localStorage.setItem(prefKey('ui.work_view'), 'on');
localStorage.setItem('wtm.view', 'work');
document.documentElement.dataset.theme = params.get('theme') ?? 'light';
document.documentElement.dataset.palette = 'pine';
document.documentElement.dataset.platform = 'macos';
mockWindows('main');
const KEY = 'ACME-301';
const FLOW = 40;
const REVIEW = 41;
const WT = '/x/' + KEY;
const project = { id: '/x/webapp', name: 'webapp', root: '/x/webapp', usable: true, problem: null, trust: null };
const wt = (dir, branch) => ({
  id: dir === 'webapp' ? '/x/webapp' : '/x/' + dir, title: dir, subtitle: branch ?? '(detached)',
  path: dir === 'webapp' ? '/x/webapp' : '/x/' + dir, dirname: dir, branch, head: 'a1b2c3d4e5',
  isMain: dir === 'webapp', isBare: false, locked: null, prunable: null, dirty: false, untracked: 0,
  staged: 0, ahead: 0, behind: 0, issueKey: null, badges: [], links: [], table: [], env: [],
  browserHome: null });
const worktrees = [wt('webapp', 'main'), wt(KEY, 'feature/' + KEY)];
const status = { name: 'In Progress', category: 'in_progress' };
const issue = { key: KEY, summary: 'Export the unit list', status, kind: 'Story', priority: 'High', assignee: null, labels: [] };
const sprint = { id: 41, name: 'Sprint 41', state: 'active', goal: null, startsAt: null, endsAt: null, board: 3 };
const columns = [{ role: 'todo', status: 'To Do' }, { role: 'started', status: 'In Progress' }, { role: 'done', status: 'Done' }];
const now = Date.now();

/* Rust's record, kept as the engine keeps it. */
const state = {
  ready: { phase: 'ready_to_test', hold: null },
  review: { phase: 'in_review', hold: null },
  interrupted: { phase: 'fixing', hold: { kind: 'interrupted', cause: 'quit' } },
  question: { phase: 'implementing', hold: { kind: 'awaiting_answer' } },
  none: { phase: 'implementing', hold: null },
}[scenario];
const flowView = () => ({ id: FLOW, workItem: FLOW, state: { kind: 'implement', phase: state.phase, hold: state.hold, end: null },
  seq: 9, startedAt: now, updatedAt: now });
const claim = (criterion, met, evidence) => ({ criterion, met, evidence });
const handedIn = { 1: { summary: 'Added the exporter and its page', acceptance: [claim(1, false, 'one unit is still missing')] },
  2: { summary: 'Fixed the off-by-one', acceptance: [claim(1, true, 'tests/export_test.py::test_every_unit')] },
  3: { summary: 'Sorted the units; the order needs a decision', acceptance: [] } };
const round = (n, kind, findings, checks, ended) => ({ flow: FLOW, round: n, kind, planRevision: 1,
  findings: findings.map((f) => ({ finding: f, title: 'Off by one', location: null })), checks,
  headSha: ended ? 'c' + n + 'e4f5a6b7' : null, changed: ['src/export.rs'], note: null,
  handoff: ended ? { changed: [], checks: [], openQuestions: [], ...handedIn[n] } : null, startedAt: now, endedAt: ended ? now : null });
const rounds = scenario === 'none' ? [] : scenario === 'question' ? [round(1, 'implement', [], [], false)]
  : scenario === 'interrupted' ? [round(1, 'implement', [], [], true), round(2, 'fix', [7], ['src-test'], false)]
  : [round(1, 'implement', [], [], true), round(2, 'fix', [7], ['src-test'], true), round(3, 'fix', [9], [], true)];
const handedOver = scenario === 'ready' || scenario === 'review';
const commit = (n, sha, message, by, paths) => ({ flow: FLOW, round: n, commit: sha, message, paths, by, madeAt: now });
const checkpoints = scenario === 'none' || scenario === 'question' ? [] : [
  commit(1, 'a1f0c3d9e8b7', 'Export the units', 'writer', ['src/export.rs', 'docs/export.md']),
  commit(2, 'b2e1d4c0f9a8', 'Export every unit', 'writer', ['src/export.rs']),
  ...(handedOver ? [commit(3, 'c3d2e5b1a0f9', 'ACME-301: fix what the review found', 'app', ['src/export.rs'])] : []),
];
const check = (id, name, status, detail) => ({ id, check: name, role: null, status, detail, exitCode: status === 'passed' ? 0 : status === 'failed' ? 1 : null,
  treeSha: 't' + id, cwd: WT, services: [], startedAt: now, endedAt: now });
const checks = scenario === 'none' || scenario === 'question' ? [] : [
  check(1, 'lint', 'passed', null), check(2, 'src-test', 'failed', null), check(3, 'e2e', 'blocked', 'blocked: environment not started: web is stopped'),
  check(4, 'lint', 'passed', null), check(5, 'src-test', 'passed', null),
];
const implementation = scenario === 'none' ? null : {
  flow: FLOW, project: '/x/webapp', worktree: WT, key: KEY, role: 'implementer', recipe: 'standard', baseSha: 'a1b2c3d4e5',
  planRevision: 1, scope: { areas: ['src/**', 'docs/**'], files: ['tests/export_test.py'] }, bounds: { rounds: 2, attempts: 1 },
  reviewFlow: REVIEW, outcome: handedOver ? 'ready' : null,
  remaining: handedOver ? [
    { finding: 9, title: 'Units are exported out of order', location: { path: 'src/export.rs', startLine: 12, endLine: 14 }, because: { kind: 'rounds_spent' } },
    { finding: 11, title: 'The README says nothing exports', location: { path: 'README.md', startLine: 1, endLine: 1 }, because: { kind: 'outside_plan', paths: ['README.md'] } },
  ] : [],
  stopped: handedOver ? { kind: 'bounded', rounds: 2 } : null, startedAt: now, updatedAt: now };
const question = { id: 70, flow: FLOW, step: 5, role: 'implementer', text: '\`implementer\` changed \`README.md\` outside the approved plan. Change the plan to include it?',
  why: 'Plan revision 1 covers \`src/**\`, \`docs/**\`.', unblocks: 'Committing those files.', blocks: 'verifying', context: [],
  recommendation: null, options: ['Change the plan to include them', 'Keep to the plan'], askedAt: now, answer: null };
let resumed = false;
const implementView = () => ({ flow: flowView(), key: KEY, implementer: scenario === 'none' ? null : 'implementer',
  implementation: structuredClone(implementation), rounds: structuredClone(rounds), checkpoints: structuredClone(checkpoints),
  checks: structuredClone(checks), verifying: ['lint', 'src-test', 'e2e'],
  review: implementation ? { flow: REVIEW, phase: 'consolidated', running: false, standing: handedOver ? 2 : 0 } : null,
  findings: [], questions: scenario === 'question' ? [{ link: { question: 70, flow: FLOW, kind: 'outliers', paths: ['README.md'], findings: [], planRevision: 1 }, question }] : [],
  sessions: [], step: null, lease: handedOver || scenario === 'none' ? null : { id: 3, worktree: WT, flow: FLOW, kind: 'writer', holder: 'implementer', acquiredAt: now, releasedAt: null, releasedWhy: null },
  running: resumed, canStart: false, canResume: scenario === 'interrupted' && !resumed });
const item = (id, kind, title, body) => ({ id, flow: FLOW, kind, target: { kind: 'flow' }, title, body,
  state: 'open', createdAt: now, closedAt: null, project: '/x/webapp', worktree: WT, issue: KEY, question: null });
const inbox = scenario === 'ready' ? [item(1, 'review_ready', KEY + ' is ready to test',
    '3 commits by the app on \`feature/' + KEY + '\`, 2 fix rounds. Checks: e2e blocked: environment not started, lint passed, src-test passed. The fix loop stopped at its bound of 2 rounds.')]
  : scenario === 'review' ? [item(3, 'feedback', KEY + ': new feedback on #40',
    'sam reviewed it. Start a revision from the issue\\'s Implement tab: the implementer gets the feedback in its same conversation.')]
  : scenario === 'interrupted' ? [item(2, 'interrupted', KEY + ': wtm quit while it was being implemented',
    'Nothing was sent again. The writer still holds the issue\\'s worktree. Resume carries it on where it stopped.')] : [];
const changed = () => setTimeout(() => emit('flow:changed', { flow: FLOW }), 0);
/* The hand-over, as handoff_view reads it: a docs edit by hand since the evidence was gathered. */
const handoffView = () => ({ flow: FLOW, key: KEY, project: '/x/webapp', worktree: WT, branch: 'feature/' + KEY,
  baseSha: 'a1b2c3d4e5', headSha: 'c3d2e5b1a0f9', files: [{ path: 'docs/export.md', added: 4, removed: 0 }, { path: 'src/export.rs', added: 31, removed: 2 }],
  patch: 'diff --git a/src/export.rs b/src/export.rs\\n+pub fn export(units: &[Unit]) -> String {\\n', truncated: false, uncommitted: ['docs/export.md'],
  coverage: [{ criterion: 1, text: 'The export lists every unit', claim: claim(1, true, 'tests/export_test.py::test_every_unit'), findings: [] },
    { criterion: 2, text: 'Units come out in the order the page shows them', claim: null, findings: [9] }],
  checks: [{ run: checks[2], tail: 'web is stopped', stale: true }, { run: checks[3], tail: 'ok', stale: true },
    { run: checks[4], tail: 'test result: ok. 12 passed', stale: false }],
  since: { paths: ['docs/export.md'], complete: true, findings: [] }, canRevise: !revised });
const environmentView = () => ({ project: '/x/webapp', worktree: WT, declared: true,
  services: [{ id: 'web', label: 'Web', state: 'healthy', detail: null, ports: [], health: 'http://localhost:18431/healthz',
    preview: 'http://localhost:18431', detached: false, pid: 4242, leftRunning: false, conflicts: [], startedAt: now, updatedAt: now }],
  resources: [], commands: [], runs: [], idleStopMinutes: null, problem: null });
let revised = false;
/* The issue's own pull request, as own_pr_view keeps it. */
const HEAD = 'c3d2e5b1a0f9d8c7b6a5f4e3d2c1b0a9f8e7d6c5';
const op = (id, position, payload, state, result) => ({ id, key: { flow: FLOW, kind: payload.kind, target: 'acme/webapp#40', payloadHash: 'h' + id },
  approval: 0, position, payload, state, attempts: 1, lastError: null, result, attention: null, resolution: null, createdAt: now, updatedAt: now });
const repoSlug = { host: 'github.com', owner: 'acme', name: 'webapp' };
const draftSend = { approval: { id: 61, flow: FLOW, intent: 'route', grantedAt: now },
  request: { kind: 'draft', request: { key: KEY, branch: 'feature/' + KEY, base: 'develop', commit: HEAD, pullRequest: null } },
  operations: [op(1, 0, { kind: 'push_branch', route: 'r', destination: {}, commit: HEAD, expect: { kind: 'absent' } }, 'done', { kind: 'pushed', url: '', reference: 'refs/heads/feature/' + KEY, commit: HEAD, adopted: false }),
    op(2, 1, { kind: 'create_draft_pull_request', draft: 'r', repo: repoSlug, base: 'develop', head: 'feature/' + KEY, title: 't', body: 'b' }, 'done', { kind: 'pull_request', number: 40, url: 'https://github.com/acme/webapp/pull/40', adopted: false })],
  state: 'published' };
const readySend = { approval: { id: 62, flow: FLOW, intent: 'publish', grantedAt: now },
  request: { kind: 'ready', request: { key: KEY, ready: { number: 40, head: HEAD }, toReview: { key: KEY, from: 'In Progress', to: 'In Review' } } },
  operations: [op(3, 0, { kind: 'mark_ready', repo: repoSlug, number: 40, head: HEAD }, 'done', { kind: 'ready', number: 40, url: '', adopted: false }),
    op(4, 1, { kind: 'move_issue', tracker: 'jira', key: KEY, from: 'In Progress', to: 'In Review' }, 'done', { kind: 'moved', key: KEY, status: 'In Review', adopted: false })],
  state: 'published' };
const prReading = { number: 40, url: 'https://github.com/acme/webapp/pull/40', title: KEY + ': Export the unit list', author: 'you', state: 'open',
  draft: scenario !== 'review', head: { repo: repoSlug, branch: 'feature/' + KEY, sha: HEAD }, base: { repo: repoSlug, branch: 'develop', sha: null },
  crossRepository: false, maintainerCanModify: true, requestedReviewers: [], checks: { passed: 3, failed: 0, pending: 0, skipped: 0 },
  reviewDecision: scenario === 'review' ? 'changes_requested' : null, mergeState: scenario === 'review' ? 'clean' : 'draft' };
const pullView = () => ({ flow: FLOW, unavailable: null, branch: 'feature/' + KEY, base: 'develop', repo: 'acme/webapp',
  pullRequest: { repo: 'acme/webapp', number: 40 }, url: 'https://github.com/acme/webapp/pull/40',
  reading: scenario === 'review' ? { item: FLOW, pullRequest: prReading, permission: 'admin', readAt: now } : null,
  sends: scenario === 'review' ? [draftSend, readySend] : [draftSend],
  feedback: scenario === 'review' ? [{ flow: FLOW, review: 501, author: 'sam', state: 'changes_requested', body: 'Sort the units by floor, please.',
    url: 'https://github.com/acme/webapp/pull/40#pullrequestreview-501', commitId: HEAD, comments: [{ path: 'src/export.rs', body: 'This loop should sort first.' }], seenAt: now, revisedIn: null }] : [],
  canPublish: scenario === 'ready', canMerge: scenario === 'review', canSync: true, canReviseFeedback: scenario === 'review' });
window.calls = [];
mockIPC((cmd, args) => {
  const a = args ?? {};
  window.calls.push({ cmd, args: structuredClone(a) });
  switch (cmd) {
    case 'get_pref': return localStorage.getItem(prefKey(a.key));
    case 'set_pref': localStorage.setItem(prefKey(a.key), a.value); return null;
    case 'list_projects': return [structuredClone(project)];
    case 'list_worktrees': return structuredClone(worktrees);
    case 'sidebar_layout': return { groups: [], origins: {} };
    case 'list_agents': return [];
    case 'home_jobs': return [];
    case 'list_openers': return { openers: [], preferred: null };
    case 'usage_limits': return [];
    case 'browser_available': return { runtime: false, reason: 'harness' };
    case 'notification_permission': return 'granted';
    case 'dictation_status': return { available: false, recording: false, key: null, reason: 'harness' };
    case 'doctor': return { path: [], tools: [], problems: [] };
    case 'review_queue': return { forge: null, items: [] };
    case 'tracker_queue':
    case 'sync_tracker': {
      const queue = { tracker: 'jira', query: 'assignee = currentUser()', board: 3, columns, sprints: [sprint],
        issues: [{ tracker: 'jira', issue: structuredClone(issue), sprint: 41, column: 1, role: 'started', workItem: FLOW,
          worktrees: [WT], run: { flow: FLOW, worktree: WT, state: flowView().state,
            openItems: inbox.length, start: 'ready' }, section: inbox.length > 0 ? 'needs_you' : 'running' }],
        readAt: now, failure: null, outdated: false };
      return cmd === 'sync_tracker' ? { queue, report: null } : queue;
    }
    case 'tracker_issue':
      return { tracker: 'jira', readAt: now, column: 1, role: 'started', account: null,
        detail: { issue: structuredClone(issue), description: 'Export the units as CSV.',
          parent: null, updated: null, sprints: [sprint], links: [], fields: [] } };
    case 'issue_start':
      return { workItem: FLOW, flow: flowView(), job: { workItem: FLOW, flow: FLOW, project: '/x/webapp', key: KEY,
        candidate: { kind: 'new', branch: 'feature/' + KEY, directory: WT, base: 'main' }, worktree: WT,
        branch: 'feature/' + KEY, state: 'ready', detail: null, setupSession: null, createdAt: now, updatedAt: now },
        working: false, moves: [], inbox: structuredClone(inbox), startMove: { kind: 'stay', reason: 'already_there' },
        startStatus: 'In Progress' };
    case 'issue_plan': return null;
    case 'issue_design': return null;
    case 'issue_implementation': return implementView();
    case 'resume_implementing':
      resumed = true;
      state.hold = null;
      inbox.splice(0, inbox.length);
      changed();
      return null;
    case 'work_inbox': return structuredClone(inbox);
    case 'implementation_handoff': return handoffView();
    case 'issue_pull_request':
    case 'sync_pull_request': return pullView();
    case 'ready_sheet': return { flow: FLOW, key: KEY, repo: 'acme/webapp', number: 40, url: prReading.url, draft: true, head: HEAD,
      handedOver: HEAD, trackerFrom: 'In Progress', trackerTo: 'In Review', problems: [] };
    case 'merge_sheet': return { flow: FLOW, key: KEY, repo: 'acme/webapp', number: 40, url: prReading.url, branch: 'feature/' + KEY,
      base: 'develop', head: HEAD, pullRequest: prReading, methods: ['squash', 'merge', 'rebase'], problems: [] };
    case 'approve_ready':
    case 'approve_merge':
    case 'revise_from_feedback': return null;
    case 'environment': return environmentView();
    case 'revise_implementation':
      revised = true;
      changed();
      return null;
    default: return cmd.startsWith('list_') ? [] : null;
  }
}, { shouldMockEvents: true });
await import('@xterm/xterm/css/xterm.css');
await import('/src/styles/main.scss');
const { mount } = await import('svelte');
const { default: App } = await import('/src/App.svelte');
const { view: views } = await import('/src/lib/state/view.svelte.ts');
const { workspace } = await import('/src/lib/state/workspace.svelte.ts');
const { work } = await import('/src/lib/state/work.svelte.ts');
window.harness = { emit, view: views, workspace, work };
mount(App, { target: document.getElementById('app') });`;

const server = await createServer({
    configFile: false,
    envDir: false,
    logLevel: 'warn',
    plugins: [
        svelte(),
        {
            name: 'implement-probe',
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

    async function open(query, colorScheme) {
        const context = await browser.newContext({
            viewport: { width: 1280, height: 900 },
            colorScheme,
        });
        const page = await context.newPage();
        page.on('pageerror', (e) => {
            throw new Error(`page error: ${e.message}`);
        });
        page.on('console', (message) => {
            if (message.type() === 'error') console.error('CONSOLE', message.text());
        });
        await page.goto(base + query);
        await page.waitForFunction(() => window.harness?.workspace.worktrees.length > 0);
        await page.waitForFunction(() => window.harness.view.current === 'work');
        await queue(page).waitFor({ state: 'visible' });
        await queue(page).getByText('ACME-301').waitFor();
        return page;
    }

    const shot = (page, path) => page.screenshot({ path, animations: 'disabled' });
    const calls = (page, cmd) =>
        page.evaluate((c) => window.calls.filter((x) => x.cmd === c), cmd);
    const queue = (page) => page.getByRole('navigation', { name: 'Work queue' });
    const row = (page, key) => queue(page).locator('.c-work-row').filter({ hasText: key });
    const panel = (page) => page.getByRole('tabpanel');
    const tab = (page) => panel(page).locator('.c-implement');
    const openInbox = (page) =>
        page
            .getByRole('button', { name: /^Inbox/ })
            .first()
            .click();

    for (const scheme of ['light', 'dark']) {
        // ── Ready to test: the item leads to the Implement tab, not to a pause ───────────────
        const page = await open(`?theme=${scheme}`, scheme);
        await openInbox(page);
        const ready = page
            .locator('.c-inbox-card')
            .filter({ hasText: 'ACME-301 is ready to test' });
        await ready.waitFor();
        assert.equal(await ready.getByRole('button', { name: 'Pause the run' }).count(), 0);
        await shot(page, join(shots, `inbox-ready-to-test-${scheme}.png`));
        await ready.getByRole('button', { name: 'See what was done' }).click();
        await tab(page).waitFor();
        await tab(page).locator('.c-implement__title').getByText('Ready to test').waitFor();

        // Why it stopped, and what it left with why.
        const handover = tab(page).locator('.c-handoff');
        await handover
            .getByText('The fix loop stopped at its bound of 2 rounds.')
            .waitFor();
        const left = await handover.innerText();
        for (const part of [
            'Units are exported out of order',
            'the fix rounds were spent',
            'The README says nothing exports',
            'outside the plan (README.md): fixing it needs a plan change',
        ])
            assert(left.includes(part), `the hand-over shows “${part}”:\n${left}`);

        // Each round with its commits.
        const rounds = tab(page).locator('.c-implement__round');
        assert.equal(await rounds.count(), 3);
        assert.match(
            await rounds.nth(0).innerText(),
            /Implemented plan revision 1[\s\S]*a1f0c3d/,
        );
        assert.match(
            await rounds.nth(1).innerText(),
            /Fixed 1 finding and src-test failing/,
        );
        assert.match(await rounds.nth(2).innerText(), /what it left/);

        // Each check by its newest record; the blocked one is blocked, not passed.
        const check = (name) =>
            tab(page).locator('.c-implement__check').filter({ hasText: name });
        assert.equal(await check('src-test').getAttribute('data-status'), 'passed');
        assert.match(await check('src-test').innerText(), /2 runs/);
        assert.equal(await check('e2e').getAttribute('data-status'), 'blocked');
        assert.match(await check('e2e').innerText(), /blocked: environment not started/);
        assert.doesNotMatch(await check('e2e').innerText(), /passed/);
        assert.match(await tab(page).innerText(), /has let the worktree go/);

        // The hand-over panel: each round's words, each criterion with the writer's word as its
        // word and the finding that cites it, the evidence gone stale with the docs edit, the
        // diff, the environment's controls, and Revise in your words.
        const said = await handover.innerText();
        for (const part of [
            'Round 2 · Fixed 1 finding and src-test failing',
            'Fixed the off-by-one',
            'The export lists every unit',
            'The writer says met',
            'tests/export_test.py::test_every_unit',
            'Units come out in the order the page shows them',
            'The writer said nothing about it.',
            'Cited by F9, still standing',
            'Changed since the checks ran and the review read it: docs/export.md',
            '2 files from a1b2c3d to c3d2e5b on feature/ACME-301',
            'Not committed: docs/export.md',
        ])
            assert(said.includes(part), `the hand-over panel shows “${part}”:\n${said}`);
        const evidence = (name) =>
            handover.locator('.c-handoff__check').filter({ hasText: name });
        assert.match(await evidence('lint').innerText(), /stale: changed since/);
        assert.doesNotMatch(await evidence('src-test').innerText(), /stale/);
        assert.equal(await evidence('e2e').getAttribute('data-status'), 'blocked');
        await handover
            .getByRole('group', { name: 'Environment' })
            .getByText('Env: Healthy')
            .waitFor();
        const revise = handover.getByRole('button', { name: 'Revise' });
        assert(await revise.isDisabled(), 'Revise waits for your words');
        await shot(page, join(shots, `implement-ready-${scheme}.png`));
        await handover
            .getByLabel('What should change?')
            .fill('Sort the units by building, then floor');
        await revise.click();
        await page.waitForFunction(() =>
            window.calls.some((c) => c.cmd === 'revise_implementation'),
        );
        assert.deepEqual((await calls(page, 'revise_implementation'))[0].args, {
            flow: 40,
            note: 'Sort the units by building, then floor',
        });

        // The draft the hand-over made, and Mark ready + Needs Review behind its sheet.
        const pr = tab(page).locator('.c-own-pr');
        await pr.getByText('Draft of c3d2e5b').waitFor();
        assert.match(await pr.innerText(), /Open the draft pull request: done/);
        assert.match(await pr.innerText(), /feature\/ACME-301 → develop/);
        await pr.getByRole('button', { name: 'Mark ready + Needs Review…' }).click();
        const readySheet = page.getByRole('dialog');
        await readySheet.getByText('(the hand-over you tested)').waitFor();
        assert.match(await readySheet.innerText(), /#40: draft → ready/);
        assert.match(await readySheet.innerText(), /ACME-301: In Progress → In Review/);
        await shot(page, join(shots, `ready-sheet-${scheme}.png`));
        await readySheet.getByRole('button', { name: 'Approve both' }).click();
        await page.waitForFunction(() =>
            window.calls.some((c) => c.cmd === 'approve_ready'),
        );
        assert.deepEqual((await calls(page, 'approve_ready'))[0].args, {
            flow: 40,
            head: 'c3d2e5b1a0f9d8c7b6a5f4e3d2c1b0a9f8e7d6c5',
        });
        await page.context().close();

        // ── In review: what GitHub said, the feedback once, a revision from it, the Merge sheet ──
        const followed = await open(`?theme=${scheme}&scenario=review`, scheme);
        await openInbox(followed);
        const feedbackCard = followed
            .locator('.c-inbox-card')
            .filter({ hasText: 'new feedback on #40' });
        await feedbackCard.waitFor();
        await feedbackCard.getByRole('button', { name: 'See the feedback' }).click();
        await tab(followed).locator('.c-implement__title').getByText('In review').waitFor();
        const own = tab(followed).locator('.c-own-pr');
        await own.getByText('sam').first().waitFor();
        const ownText = await own.innerText();
        for (const part of [
            'open, ready for review · checks 3✓ 0✗ · changes requested · can merge',
            'Ready + Needs Review',
            '#40: draft → ready: done',
            'ACME-301: In Progress → In Review: done',
            'sam asked for changes',
            'Sort the units by floor, please.',
            'This loop should sort first.',
        ])
            assert(
                ownText.includes(part),
                `the pull request section shows “${part}”:\n${ownText}`,
            );
        assert.equal(await own.getByRole('button', { name: /Mark ready/ }).count(), 0);
        await shot(followed, join(shots, `implement-in-review-${scheme}.png`));
        await own.getByRole('button', { name: 'Sync' }).click();
        await followed.waitForFunction(() =>
            window.calls.some((c) => c.cmd === 'sync_pull_request'),
        );
        await own
            .getByRole('button', { name: 'Start a revision from the feedback' })
            .click();
        await followed.waitForFunction(() =>
            window.calls.some((c) => c.cmd === 'revise_from_feedback'),
        );
        assert.deepEqual((await calls(followed, 'revise_from_feedback'))[0].args, {
            flow: 40,
        });
        await own.getByRole('button', { name: 'Merge…' }).click();
        const mergeSheet = followed.getByRole('dialog');
        await mergeSheet.getByText('Only this sheet merges.', { exact: false }).waitFor();
        assert.match(await mergeSheet.innerText(), /feature\/ACME-301 → develop/);
        await mergeSheet.getByLabel('Merge method').selectOption('rebase');
        await shot(followed, join(shots, `merge-sheet-${scheme}.png`));
        await mergeSheet.getByRole('button', { name: 'Merge c3d2e5b' }).click();
        await followed.waitForFunction(() =>
            window.calls.some((c) => c.cmd === 'approve_merge'),
        );
        assert.deepEqual((await calls(followed, 'approve_merge'))[0].args, {
            flow: 40,
            head: 'c3d2e5b1a0f9d8c7b6a5f4e3d2c1b0a9f8e7d6c5',
            method: 'rebase',
        });
        await followed.context().close();

        // ── Cut off by a quit: Resume from the card, once ────────────────────────────────────
        const cut = await open(`?theme=${scheme}&scenario=interrupted`, scheme);
        await openInbox(cut);
        const card = cut
            .locator('.c-inbox-card')
            .filter({ hasText: 'wtm quit while it was being implemented' });
        await card.waitFor();
        await shot(cut, join(shots, `inbox-interrupted-${scheme}.png`));
        await row(cut, 'ACME-301').click();
        await cut.getByRole('tab', { name: 'Implement' }).click();
        await tab(cut).locator('.c-implement__title').getByText('Interrupted').waitFor();
        assert.match(await tab(cut).innerText(), /under way/);
        assert.match(await tab(cut).innerText(), /holds the worktree/);
        await shot(cut, join(shots, `implement-interrupted-${scheme}.png`));
        await tab(cut).getByRole('button', { name: 'Resume' }).click();
        await cut.waitForFunction(
            () => window.calls.filter((c) => c.cmd === 'resume_implementing').length === 1,
        );
        assert.deepEqual((await calls(cut, 'resume_implementing'))[0].args, { flow: 40 });
        await tab(cut)
            .getByRole('button', { name: 'Resume' })
            .waitFor({ state: 'detached' });
        await cut.context().close();

        // ── A question about a file outside the plan waits in the Inbox ──────────────────────
        const asked = await open(`?theme=${scheme}&scenario=question`, scheme);
        await row(asked, 'ACME-301').click();
        await asked.getByRole('tab', { name: 'Implement' }).click();
        await tab(asked)
            .locator('.c-implement__title')
            .getByText('Waiting for your answer')
            .waitFor();
        await tab(asked).getByText('waiting for you in the Inbox').waitFor();
        assert.match(await tab(asked).innerText(), /README\.md/);
        await shot(asked, join(shots, `implement-question-${scheme}.png`));
        await asked.context().close();

        // ── No implementer configured: said, and nothing to start ────────────────────────────
        const none = await open(`?theme=${scheme}&scenario=none`, scheme);
        await row(none, 'ACME-301').click();
        await none.getByRole('tab', { name: 'Implement' }).click();
        await tab(none)
            .getByText(/This project names no implementer/)
            .waitFor();
        assert.equal(
            await tab(none)
                .getByRole('button', { name: /Start implementing|Resume/ })
                .count(),
            0,
            'nothing to start without an implementer',
        );
        await shot(none, join(shots, `implement-none-${scheme}.png`));
        await none.context().close();
    }

    console.log(`ok — screenshots in ${shots}`);
} finally {
    await browser?.close();
    await server.close();
}

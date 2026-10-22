/**
 * Drive the real app's Work view with mocked IPC through a local fix of a teammate's pull request:
 * the outcome sheet's Fix it locally, the writer running under its lease, the fix's diff, the three
 * routes with exactly where each goes, the preview and approval of one, and every state of a
 * route's card.
 *
 * What it proves, in order: Fix it locally offers only findings that name a file, picks a
 * confirmed bug, and sends exactly the picks and the writer role; while the writer runs the
 * Reviews tab says it holds the worktree and offers Stop fix, not Stop review; a ready fix opens
 * the sheet on its diff — files, counts, anything left uncommitted — and its routes, the push
 * route closed with its reason; Approve and send stays shut until a preview of the route as it
 * stands, an edit after the preview shuts it again, and approving sends exactly the previewed
 * request and its hash; the route's card shows sending, sent with a link, a branch that moved
 * after approval with nothing to push, a failure with Retry, and the check after a relaunch;
 * suggestions preview as GitHub suggestion blocks; and the inbox's item about a route opens the
 * same card. No Tauri, no `gh`, no git: the engine behind these shapes is
 * `src-tauri/tests/local_fixes.rs`'s.
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
const shots = process.env.WTM_SHOTS || join(tmpdir(), 'wtm-local-fix');
mkdirSync(shots, { recursive: true });

/*
 * The page: the real `App`, Work on, pull request #4031 reviewed at FIRST with a confirmed bug in
 * a file and a finding about the whole change. `window.phase` and `window.fix` are the run's state
 * as the mocked backend holds it, and `window.route` its route's send. A test changes them and
 * emits `flow:changed`.
 */
const entry = `
import { emit } from '@tauri-apps/api/event';
import { mockIPC, mockWindows } from '@tauri-apps/api/mocks';
const params = new URLSearchParams(location.search);
const prefKey = (k) => 'harness.pref.' + k;
localStorage.setItem(prefKey('ui.notify'), 'off');
localStorage.setItem(prefKey('ui.work_view'), 'on');
localStorage.setItem('wtm.view', 'work');
document.documentElement.dataset.theme = params.get('theme') ?? 'light';
document.documentElement.dataset.palette = 'pine';
document.documentElement.dataset.platform = 'macos';
mockWindows('main');
const FIRST = '1156f977ff6d12d592bc152987b9d9b378f6718d';
const FIX = 'f1a2b3c4d5e6f708090a0b0c0d0e0f1011121314';
const THEIRS = '9e0d1c2b3a4f5e6d7c8b9a0f1e2d3c4b5a6f7e8d';
const project = { id: '/x/webapp', name: 'webapp', root: '/x/webapp', usable: true, problem: null, trust: null };
const wt = (dir, branch) => ({
  id: '/x/' + dir, title: dir, subtitle: branch ?? '(detached)', path: '/x/' + dir, dirname: dir,
  branch, head: FIRST, isMain: dir === 'webapp', isBare: false, locked: null, prunable: null,
  dirty: false, untracked: 0, staged: 0, ahead: 0, behind: 0, issueKey: null, badges: [], links: [],
  table: [], env: [], browserHome: null });
const worktrees = [wt('webapp', 'main'), wt('webapp-pr-4031', null)];
const item = { id: 1, repo: 'acme/webapp', number: 4031, title: 'Export units as CSV', worktree: '/x/webapp-pr-4031', reviewingSince: null };
const repo = { host: 'github.com', owner: 'acme', name: 'webapp' };
const fork = { host: 'github.com', owner: 'sam-rivera', name: 'webapp' };
const pr = () => ({
  number: 4031, url: 'https://github.com/acme/webapp/pull/4031', title: 'Export units as CSV',
  author: 'sam-rivera', state: 'open', draft: false,
  head: { repo: fork, branch: 'feature/csv-export', sha: FIRST },
  base: { repo, branch: 'main', sha: null },
  crossRepository: true, maintainerCanModify: false, requestedReviewers: [],
  checks: { passed: 3, failed: 0, pending: 0, skipped: 0 } });
const detail = () => ({ item: structuredClone(item), pullRequest: pr(), checksState: 'passing',
  permission: 'write', canPush: true, baseIntegration: true, baseProtection: null,
  account: { host: 'github.com', login: 'dana-lee', version: '2.100.0' } });
const source = (role, provider, invocation) => ({ step: invocation, invocation, ordinal: 0, role, provider,
  model: null, workspace: 'snapshot', treeSha: 'aa11' });
const finding = (id, title, location, cls) => ({ id, flow: 9, class: cls, severity: 'notable', location,
  title, rationale: 'Each unit past the last is read as empty.', requirement: null, treeSha: 'aa11', dismissed: null, lines: true,
  evidence: [{ source: source('correctness', 'codex', 1), reported: { title, severity: 'notable', class: cls.kind, body: 'x' } }] });
const bug = finding(91, 'The export loop drops the last unit', { path: 'src/export.py', startLine: 20, endLine: 20 },
  { kind: 'confirmed_bug', verification: 'ran the export on three units and got two rows' });
const whole = finding(93, 'Nothing tests the empty export', null, { kind: 'suggestion' });
const step = (id, role, provider, profile, status) => ({ id, stage: profile === 'writer' ? 1 : 0, role, provider, model: null,
  profile, workspace: profile === 'writer' ? 'worktree' : 'snapshot', status, failures: 0, retries: 0, standsInFor: null,
  detail: null, resetsAt: null, skillProblem: null, result: null });
window.phase = 'consolidated';
window.fix = null;
window.route = null;
window.leases = [];
const fixRecord = (state, extra = {}) => ({ id: 3, flow: 9, step: 4, role: 'fixer', findings: [91],
  scope: { paths: ['src/export.py'] }, baseSha: FIRST, headSha: state === 'writing' ? null : FIX,
  diffHash: state === 'writing' ? null : 'sha256:d1ff0a1b2c3d4e5f', state, detail: null, asked: [], allowed: [],
  startedAt: Date.now(), endedAt: state === 'writing' ? null : Date.now(), ...extra });
window.fixRecord = fixRecord;
const view = () => ({
  flow: { id: 9, state: { kind: 'review_pr', phase: window.phase, hold: null, end: null }, startedAt: 0, updatedAt: 0 },
  review: { flow: 9, project: '/x/webapp', worktree: '/x/webapp-pr-4031', worktreePath: '/x/webapp-pr-4031', recipe: 'standard', base: 'origin/main', snapshot: 1 },
  snapshot: { headSha: FIRST, treeSha: 'aa11', baseSha: 'c0ffee11' },
  steps: [step(1, 'correctness', 'codex', 'reviewer', 'done'),
    ...(window.fix ? [step(4, 'fixer', 'claude', 'writer', window.phase === 'fixing' ? 'running' : 'done')] : [])],
  invocations: [], inbox: [], running: window.phase === 'fixing',
  findings: [bug, whole],
  checks: [], expectedChecks: [], applied: { rules: [], sources: ['CLAUDE.md'], skills: [] },
  pullRequest: {
    item: 1, repo: 'acme/webapp', number: 4031,
    mirror: { item: 1, pullRequest: pr(), permission: 'write', readAt: 1 },
    reviewing: null, reviewedHead: FIRST, movedTo: null,
    pushToBranch: 'fork_without_maintainer_edits', earlier: [], outcomes: [],
    publications: [], routes: window.route ? [structuredClone(window.route)] : [],
  },
  fixes: window.fix ? [structuredClone(window.fix)] : [],
  leases: structuredClone(window.leases),
  writers: [{ id: 'fixer', provider: 'claude', model: 'sonnet', objective: 'Fix the findings you are given' }],
  screenshots: [],
  pendingStart: null,
});
const PR_MARK = '<!-- wtm:5e6f7a8b9c0d1e2f -->';
const BRANCH = 'wtm/pr-4031-fix-f1a2b3c';
const plan = (request) => ({
  kind: request.kind, pullRequest: { repo, number: 4031, url: pr().url },
  their: { repo: fork, branch: 'feature/csv-export', head: FIRST },
  fix: { base: FIRST, head: FIX, diffHash: 'sha256:d1ff0a1b2c3d4e5f' },
  push: { remote: 'mine', url: 'git@github.com:dana-lee/webapp.git', repo: { host: 'github.com', owner: 'dana-lee', name: 'webapp' }, reference: 'refs/heads/' + BRANCH },
  expect: { kind: 'absent' },
  open: { repo: fork, base: 'feature/csv-export', head: 'dana-lee:' + BRANCH, title: request.title, body: request.body } });
const hashOf = (request) => 'sha256:' + Array.from(JSON.stringify(request)).reduce((h, ch) => (h * 31 + ch.charCodeAt(0)) >>> 0, 7).toString(16);
const routeOp = (id, position, kind, state, extra = {}) => ({
  id, key: { flow: 9, kind, target: 't', payloadHash: 'sha256:op' + id }, approval: 6, position,
  payload: kind === 'push_branch'
    ? { kind, route: 'sha256:r', destination: window.approvedPlan.push, commit: FIX, expect: window.approvedPlan.expect }
    : { kind, route: 'sha256:r', pullRequest: window.approvedPlan.open },
  state, attempts: state === 'pending' ? 0 : 1, lastError: null, result: null, attention: null, resolution: null,
  createdAt: Date.now(), updatedAt: Date.now(), ...extra });
window.routeSend = (states, extra = {}) => ({
  approval: { id: 6, flow: 9, intent: 'route', binding: { remote: window.approvedPlan.push.url, ref: window.approvedPlan.push.reference,
    diff: 'sha256:d1ff0a1b2c3d4e5f', commit: FIX, head: FIRST, payload: 'sha256:plan' }, grantedAt: Date.now() },
  plan: structuredClone(window.approvedPlan),
  operations: [
    routeOp(1, 0, 'push_branch', states[0], states[0] === 'done'
      ? { result: { kind: 'pushed', url: window.approvedPlan.push.url, reference: window.approvedPlan.push.reference, commit: FIX, adopted: false } }
      : (extra.first ?? {})),
    routeOp(2, 1, 'open_pull_request', states[1], states[1] === 'done'
      ? { result: { kind: 'pull_request', number: 77, url: 'https://github.com/sam-rivera/webapp/pull/77', adopted: false } }
      : (extra.second ?? {})),
  ],
  state: extra.state, url: states[1] === 'done' ? 'https://github.com/sam-rivera/webapp/pull/77' : null });
window.diff = () => ({ fix: structuredClone(window.fix), base: FIRST, head: FIX, hash: 'sha256:d1ff0a1b2c3d4e5f',
  files: [{ path: 'src/export.py', added: 1, removed: 1 }],
  patch: 'diff --git a/src/export.py b/src/export.py\\n@@ -20 +20 @@\\n-    for unit in units[:-1]:\\n+    for unit in units:\\n',
  truncated: false, uncommitted: window.left ?? [], outliers: window.left ?? [] });
window.inbox = [];
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
    case 'review_queue': return { forge: 'github', items: [structuredClone(item)] };
    case 'review_detail': return detail();
    case 'review_recipes': return [{ id: 'standard', label: 'Standard', roles: [
      { id: 'correctness', provider: 'codex', model: null, skills: [] }] }];
    case 'latest_review': return a.worktree === '/x/webapp-pr-4031' ? view() : null;
    case 'review_run': return view();
    case 'review_staleness':
      return { flow: 9, treeSha: 'aa11', headSha: FIRST, changed: false, paths: [], complete: true,
        staleFindings: [], staleChecks: [], affected: [], pullRequestHead: null, pullRequestHeadHere: true };
    case 'start_local_fix':
      window.phase = 'fixing';
      window.fix = fixRecord('writing');
      window.leases = [{ id: 1, worktree: '/x/webapp-pr-4031', flow: 9, kind: 'writer', holder: 'fixer', acquiredAt: Date.now(), releasedAt: null, releasedWhy: null }];
      return view();
    case 'cancel_local_fix':
      window.phase = 'fix_ready';
      window.fix = fixRecord('cancelled', { detail: 'You stopped it.', headSha: FIRST });
      window.leases = [];
      return view();
    case 'local_fix_diff': return window.diff();
    case 'fix_routes':
      return { fix: { base: FIRST, head: FIX, diffHash: 'sha256:d1ff0a1b2c3d4e5f' },
        their: { repo: fork, branch: 'feature/csv-export', head: FIRST }, account: 'dana-lee',
        routes: [
          { kind: 'follow_up', unavailable: null,
            destination: 'Push f1a2b3c to \`mine\` (git@github.com:dana-lee/webapp.git) as \`refs/heads/' + BRANCH + '\`, then open a pull request in sam-rivera/webapp: \`dana-lee:' + BRANCH + '\` into \`feature/csv-export\`',
            title: 'Fixes for #4031', body: 'Fixes from my review of #4031.' },
          { kind: 'suggestions', unavailable: null, destination: '1 suggestion comment and 0 in the summary, on acme/webapp#4031 at 1156f97',
            title: '', body: 'Suggested changes for #4031.' },
          { kind: 'push_to_branch', unavailable: 'Their branch is in a fork whose author has not allowed maintainers to edit it, so nobody but them can push to it.',
            destination: null, title: '', body: '' },
        ] };
    case 'preview_fix_route': {
      const request = a.request;
      if (request.kind === 'suggestions') {
        const body = 'Suggested fix: The export loop drops the last unit\\n\\n\`\`\`suggestion\\n    for unit in units:\\n\`\`\`\\n\\n<!-- wtm:1a2b3c4d5e6f7a8b c0 -->';
        return { kind: 'suggestions', hash: hashOf(request), plan: null,
          steps: ['gh api --method POST repos/acme/webapp/pulls/4031/reviews (a pending review of 1 suggestion)',
                  'gh api --method POST repos/acme/webapp/pulls/4031/reviews/<id>/events (submitted as a comment)'],
          body: null, diffHash: 'sha256:d1ff0a1b2c3d4e5f', head: FIRST,
          suggestions: { review: { repo, number: 4031, commitId: FIRST, event: 'comment', comments: [], folded: [], summary: request.body },
            hash: 'sha256:s', comments: [{ finding: 91, path: 'src/export.py', startLine: null, line: 20, body }],
            pendingBody: '<!-- wtm:1a2b3c4d5e6f7a8b -->', body: request.body + '\\n\\n<!-- wtm:9f8e7d6c5b4a3f2e -->',
            account: 'dana-lee', ownPullRequest: false, head: FIRST, url: pr().url } };
      }
      const p = plan(request);
      return { kind: request.kind, hash: hashOf(request), plan: p,
        steps: ['git ls-remote --refs -- mine refs/heads/' + BRANCH,
                'git -c core.hooksPath=/dev/null push --no-verify --porcelain -- mine ' + FIX + ':refs/heads/' + BRANCH,
                'gh api --method POST repos/sam-rivera/webapp/pulls (title, head dana-lee:' + BRANCH + ', base feature/csv-export, body below, maintainer_can_modify true)'],
        body: request.body + '\\n\\n' + PR_MARK, suggestions: null, diffHash: 'sha256:d1ff0a1b2c3d4e5f', head: FIRST };
    }
    case 'approve_fix_route': {
      if (a.hash !== hashOf(a.request)) throw { kind: 'flow', message: 'What would be sent has changed since you previewed it.', detail: null };
      window.approvedPlan = plan(a.request);
      window.route = window.routeSend(['executing', 'pending'], { state: 'posting' });
      return { kind: 'route', route: structuredClone(window.route) };
    }
    case 'retry_fix_route':
      window.route = window.routeSend(['done', 'executing'], { state: 'posting' });
      return structuredClone(window.route);
    case 'cancel_fix_route':
      window.route = window.routeSend(['done', 'cancelled'], { state: 'cancelled' });
      return structuredClone(window.route);
    case 'review_publication': throw { kind: 'flow', message: 'no publication', detail: null };
    case 'open_url': return null;
    case 'work_inbox': return structuredClone(window.inbox);
    default: return cmd.startsWith('list_') ? [] : null;
  }
}, { shouldMockEvents: true });
await import('@xterm/xterm/css/xterm.css');
await import('/src/styles/main.scss');
const { mount } = await import('svelte');
const { default: App } = await import('/src/App.svelte');
const { workspace } = await import('/src/lib/state/workspace.svelte.ts');
window.harness = { emit, workspace, FIRST, FIX, THEIRS, BRANCH };
mount(App, { target: document.getElementById('app') });`;

const server = await createServer({
    configFile: false,
    envDir: false,
    logLevel: 'warn',
    plugins: [
        svelte(),
        {
            name: 'local-fix-probe',
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

    const shot = (page, path) => page.screenshot({ path, animations: 'disabled' });
    const calls = (page, cmd) =>
        page.evaluate((c) => window.calls.filter((x) => x.cmd === c), cmd);
    const queue = (page) => page.getByRole('navigation', { name: 'Work queue' });
    const panel = (page) => page.locator('#work-pr-panel');
    const card = (page) => panel(page).locator('.c-publication');
    const moved = (page) => page.evaluate(() => window.harness.emit('flow:changed', { flow: 9 }));
    /** Put the backend's route send in `states` and tell the window the run moved. */
    async function send(page, states, extra) {
        await page.evaluate(
            ([s, e]) => {
                window.route = window.routeSend(s, e);
                return window.harness.emit('flow:changed', { flow: 9 });
            },
            [states, extra],
        );
    }
    async function cardSays(page, words) {
        await card(page).locator('.c-work-state__eyebrow', { hasText: words }).waitFor();
        return card(page).innerText();
    }

    for (const scheme of ['light', 'dark']) {
        const context = await browser.newContext({
            viewport: { width: 1280, height: 1100 },
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
        await queue(page).waitFor({ state: 'visible' });
        const { FIRST, FIX, THEIRS, BRANCH } = await page.evaluate(() => ({
            FIRST: window.harness.FIRST,
            FIX: window.harness.FIX,
            THEIRS: window.harness.THEIRS,
            BRANCH: window.harness.BRANCH,
        }));
        await queue(page).getByRole('button', { name: /#4031/ }).click();
        await page.getByRole('tab', { name: 'Reviews', selected: true }).waitFor();

        // ── The sheet: Fix it locally, findings with a file, a writer ─────────────────────────
        await panel(page).getByRole('button', { name: 'Finish review…' }).click();
        const sheet = page.getByRole('dialog', { name: 'Finish your review of #4031' });
        await sheet.waitFor();
        await sheet.getByRole('radio', { name: /Fix it locally/ }).check();
        const bugPick = sheet.getByRole('checkbox', { name: /drops the last unit/ });
        assert.equal(await bugPick.isChecked(), true, 'a confirmed bug starts picked');
        assert.equal(
            await sheet.getByRole('checkbox', { name: /Nothing tests the empty export/ }).count(),
            0,
            'a finding about the whole change names no file to hold a writer to',
        );
        let text = await sheet.innerText();
        assert(text.includes('Findings about the change as a whole name no file'), text);
        assert(text.includes('It cannot commit or push'), text);
        assert.equal(await sheet.getByRole('combobox', { name: 'Writer' }).inputValue(), 'fixer');
        await shot(page, join(shots, `fix-start-${scheme}.png`));
        await sheet.getByRole('button', { name: 'Start the fix' }).click();
        await sheet.waitFor({ state: 'detached' });
        assert.deepEqual((await calls(page, 'start_local_fix')).at(-1).args, {
            flow: 9,
            request: { findings: [91], role: 'fixer' },
        });

        // ── Fixing: the writer holds the worktree; Stop fix, not Stop review ─────────────────
        const state = panel(page).locator('.c-work-state').first();
        await state.locator('.c-work-state__eyebrow', { hasText: 'Fixing' }).waitFor();
        text = await state.innerText();
        assert(text.includes('fixer is fixing 1 finding of #4031'), text);
        assert(text.includes('It may change only src/export.py'), text);
        assert(text.includes('holds the review worktree as its writer'), text);
        await panel(page).getByRole('button', { name: 'Stop fix' }).waitFor();
        assert.equal(await panel(page).getByRole('button', { name: 'Stop review' }).count(), 0);
        await shot(page, join(shots, `fix-writing-${scheme}.png`));
        await panel(page).getByRole('button', { name: 'Stop fix' }).click();
        assert.deepEqual((await calls(page, 'cancel_local_fix')).at(-1).args, { flow: 9 });
        await state.locator('.c-work-state__eyebrow', { hasText: 'Local fix' }).waitFor();
        assert((await state.innerText()).includes('The local fix was stopped'));

        // ── Ready: the diff and the routes ──────────────────────────────────────────────────
        await page.evaluate(() => {
            window.phase = 'fix_ready';
            window.fix = window.fixRecord('ready');
            window.left = ['docs/guide.md'];
        });
        await moved(page);
        await state
            .locator('.c-work-state__eyebrow', { hasText: 'Local fix ready' })
            .waitFor();
        assert((await state.innerText()).includes(`committed as ${FIX.slice(0, 7)}`));
        await panel(page)
            .getByRole('button', { name: 'Read the diff and choose where it goes…' })
            .click();
        await sheet.waitFor();
        assert.equal(await sheet.getByRole('radio', { name: /Fix it locally/ }).isChecked(), true);
        const fixPart = sheet.getByRole('region', { name: 'The fix' });
        await fixPart.getByRole('group', { name: 'Where it goes' }).waitFor();
        text = await fixPart.innerText();
        for (const words of [
            `as ${FIX.slice(0, 7)} on ${FIRST.slice(0, 7)}`,
            'src/export.py',
            '+1',
            '−1',
            'Not in the fix: docs/guide.md',
            'outside the findings',
            'Follow-up pull request (default)',
            `as refs/heads/${BRANCH}`,
            'Unavailable: Their branch is in a fork',
        ])
            assert(text.includes(words), `the fix part says “${words}”:\n${text}`);
        assert.equal(
            await fixPart.getByRole('radio', { name: /Push to their branch/ }).isDisabled(),
            true,
        );
        assert.equal(
            await sheet.getByRole('textbox', { name: 'Pull request title' }).inputValue(),
            'Fixes for #4031',
        );
        await fixPart.getByText('Read the diff').click();
        await fixPart.getByText('+    for unit in units:').waitFor();
        const approve = sheet.getByRole('button', { name: 'Approve and send' });
        assert.equal(await approve.isDisabled(), true, 'nothing to approve before a preview');
        await shot(page, join(shots, `fix-ready-${scheme}.png`));

        // ── Preview exactly what is sent, then approve ──────────────────────────────────────
        await sheet
            .getByRole('textbox', { name: 'Pull request description' })
            .fill('Fixes the dropped unit.');
        await sheet.getByRole('button', { name: 'Preview exactly what is sent' }).click();
        const preview = sheet.getByRole('region', { name: 'What is sent' });
        await preview.waitFor();
        const request = { kind: 'follow_up', title: 'Fixes for #4031', body: 'Fixes the dropped unit.' };
        assert.deepEqual((await calls(page, 'preview_fix_route')).at(-1).args, {
            flow: 9,
            request,
        });
        text = await preview.innerText();
        for (const words of [
            'Exactly what is sent, in order',
            'push --no-verify --porcelain -- mine',
            `${FIX}:refs/heads/${BRANCH}`,
            'Pull request: Fixes for #4031',
            '<!-- wtm:5e6f7a8b9c0d1e2f -->',
        ])
            assert(text.includes(words), `the preview says “${words}”:\n${text}`);
        assert(!text.includes('--force'), 'no force anywhere');
        assert.equal(await approve.isDisabled(), false);
        await shot(page, join(shots, `fix-preview-${scheme}.png`));
        await sheet.getByRole('textbox', { name: 'Pull request title' }).fill('Fixes');
        await preview.getByText('You changed something since this preview').waitFor();
        assert.equal(await approve.isDisabled(), true);
        await sheet.getByRole('textbox', { name: 'Pull request title' }).fill('Fixes for #4031');
        await sheet.getByRole('button', { name: 'Preview exactly what is sent' }).click();
        await preview.getByText('Exactly what is sent').waitFor();
        await approve.click();
        await sheet.waitFor({ state: 'detached' });
        const approved = (await calls(page, 'approve_fix_route')).at(-1).args;
        assert.deepEqual(approved.request, request, 'exactly the request previewed');
        assert.match(approved.hash, /^sha256:/);

        // ── The route's card: sending, sent, moved, failed, checking ─────────────────────────
        text = await cardSays(page, 'Sending');
        assert(text.includes(`Pushing to ${BRANCH}…`), text);
        assert(text.includes('git@github.com:dana-lee/webapp.git'), text);
        assert(text.includes('diff d1ff0a1b2c3d'), text);
        await send(page, ['done', 'done'], { state: 'published' });
        text = await cardSays(page, 'Sent');
        assert(text.includes('Follow-up pull request for #4031 opened'), text);
        assert(text.includes(`Pushed to ${BRANCH}`), text);
        assert(text.includes('Pull request into feature/csv-export opened'), text);
        await card(page).getByRole('button', { name: 'View the pull request' }).click();
        assert.equal(
            (await calls(page, 'open_url')).at(-1).args.url,
            'https://github.com/sam-rivera/webapp/pull/77',
        );
        await shot(page, join(shots, `route-sent-${scheme}.png`));

        await send(page, ['needs_attention', 'pending'], {
            state: 'stale',
            first: { attention: { kind: 'head_moved', approved: FIRST, current: THEIRS } },
        });
        text = await cardSays(page, 'Approval is stale');
        assert(text.includes('Their branch moved after you approved'), text);
        assert(
            text.includes(
                `feature/csv-export moved to ${THEIRS.slice(0, 7)} after you approved it at ${FIRST.slice(0, 7)}`,
            ),
            text,
        );
        assert(text.includes('wtm never forces'), text);
        assert.equal(await card(page).getByRole('button', { name: 'Retry' }).count(), 0);
        await shot(page, join(shots, `route-moved-${scheme}.png`));

        await send(page, ['done', 'failed'], {
            state: 'failed',
            second: { lastError: 'error connecting to api.github.com' },
        });
        text = await cardSays(page, 'Partly sent');
        assert(text.includes('Pull request into feature/csv-export not opened'), text);
        assert(text.includes('error connecting to api.github.com'), text);
        await card(page).getByRole('button', { name: 'Retry' }).click();
        assert.deepEqual((await calls(page, 'retry_fix_route')).at(-1).args, { approval: 6 });

        await send(page, ['executing', 'pending'], { state: 'checking' });
        text = await cardSays(page, 'Checking the remote');
        assert(text.includes('checking the remote…'), text);
        assert.equal(await card(page).getByRole('button').count(), 0);
        await shot(page, join(shots, `route-checking-${scheme}.png`));

        // ── Suggestions preview as GitHub suggestion blocks ─────────────────────────────────
        await page.evaluate(() => (window.route = null));
        await moved(page);
        await panel(page)
            .getByRole('button', { name: 'Read the diff and choose where it goes…' })
            .click();
        await sheet.waitFor();
        await sheet.getByRole('radio', { name: /Suggestion comments/ }).check();
        await sheet.getByRole('button', { name: 'Preview exactly what is sent' }).click();
        await preview.getByText('src/export.py:20').waitFor();
        text = await preview.innerText();
        assert(text.includes('```suggestion'), text);
        assert(text.includes('Summary, submitted with them'), text);
        await shot(page, join(shots, `fix-suggestions-${scheme}.png`));
        await sheet.getByRole('button', { name: 'Cancel' }).last().click();
        await sheet.waitFor({ state: 'detached' });

        // ── The inbox's item about a route opens the same card ─────────────────────────────
        await page.evaluate(([first, theirs]) => {
            window.route = window.routeSend(['needs_attention', 'pending'], {
                state: 'stale',
                first: { attention: { kind: 'head_moved', approved: first, current: theirs } },
            });
            window.inbox = [
                {
                    id: 41,
                    flow: 9,
                    kind: 'approval',
                    target: { kind: 'approval', approval: 6 },
                    title: 'Sending your fix of #4031 stopped: their branch moved after you approved',
                    body: 'Nothing was pushed.',
                    state: 'open',
                    createdAt: Date.now(),
                    closedAt: null,
                    project: '/x/webapp',
                    worktree: '/x/webapp-pr-4031',
                    question: null,
                },
            ];
            return window.harness
                .emit('flow:changed', { flow: 9 })
                .then(() => window.harness.emit('flow:inbox', window.inbox[0]));
        }, [FIRST, THEIRS]);
        await page.getByRole('button', { name: /^Inbox/ }).click();
        const inboxCard = page.locator('.c-inbox-card');
        await inboxCard.locator('.c-publication').waitFor();
        text = await inboxCard.innerText();
        assert(text.includes('their branch moved after you approved'), text);
        assert(text.includes('Nothing was pushed over it'), text);
        await shot(page, join(shots, `route-inbox-${scheme}.png`));
        await context.close();
    }

    console.log(`ok — screenshots in ${shots}`);
} finally {
    await browser?.close();
    await server.close();
}

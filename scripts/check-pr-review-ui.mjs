/**
 * Drive the real app's Work view with mocked IPC through a teammate's pull request reviewed: Run
 * Review from its Reviews tab, the outcome sheet, Keep private notes, the author pushing, Re-review
 * changes, the earlier head kept, and "I'm reviewing this".
 *
 * What it proves, in order: a queued pull request with a review worktree opens on Reviews, whose
 * Run Review asks for a pull request review carrying the head that was shown; a reviewed one says
 * which head it read and that the pull request is still there; the outcome sheet offers Keep
 * private notes and draws Publish and Local fixes disabled, saying they come next, with the push
 * to a fork without maintainer edits unavailable; keeping notes sends them and shows the outcome;
 * a new head read from the forge makes the review stale with Re-review changes as its one action
 * (no Rerun affected, which would read the old head), and the re-review sends the new head; the
 * head it left is listed with what became of each finding; the flag is sent and shown in the
 * queue. No Tauri, no `gh`, no CLI: the engine behind these shapes is
 * `src-tauri/tests/pull_request_reviews.rs`'s.
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
const shots = process.env.WTM_SHOTS || join(tmpdir(), 'wtm-pr-review');
mkdirSync(shots, { recursive: true });

/*
 * The page: the real `App`, Work on, a queue with pull request #4031 and its review worktree. The
 * mocked backend moves through the review's life as the test asks: `window.phase` is `none`,
 * `reviewed`, `kept`, `moved` (the forge now reports a second head) or `rereviewed`.
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
const SECOND = '9e0d1c2b3a4f5e6d7c8b9a0f1e2d3c4b5a6f7e8d';
const project = { id: '/x/webapp', name: 'webapp', root: '/x/webapp', usable: true, problem: null, trust: null };
const wt = (dir, branch, extra = {}) => ({
  id: '/x/' + dir, title: dir, subtitle: branch ?? '(detached)', path: '/x/' + dir, dirname: dir,
  branch, head: FIRST, isMain: dir === 'webapp', isBare: false, locked: null, prunable: null,
  dirty: false, untracked: 0, staged: 0, ahead: 0, behind: 0, issueKey: null, badges: [], links: [],
  table: [], env: [], browserHome: null, ...extra });
const worktrees = [wt('webapp', 'main'), wt('webapp-pr-4031', null)];
const item = { id: 1, repo: 'acme/webapp', number: 4031, title: 'Export units as CSV', worktree: '/x/webapp-pr-4031', reviewingSince: null };
const pr = (head) => ({
  number: 4031, url: 'https://github.com/acme/webapp/pull/4031', title: 'Export units as CSV',
  author: 'sam-rivera', state: 'open', draft: false,
  head: { repo: { host: 'github.com', owner: 'sam-rivera', name: 'webapp' }, branch: 'feature/csv-export', sha: head },
  base: { repo: { host: 'github.com', owner: 'acme', name: 'webapp' }, branch: 'main', sha: null },
  crossRepository: true, maintainerCanModify: false, requestedReviewers: [{ name: 'dana-lee', team: false }],
  checks: { passed: 3, failed: 0, pending: 0, skipped: 0 } });
window.phase = 'none';
const forgeHead = () => (['moved', 'rereviewed'].includes(window.phase) ? SECOND : FIRST);
const detail = () => ({ item: structuredClone(item), pullRequest: pr(forgeHead()), checksState: 'passing',
  permission: 'write', canPush: true, baseIntegration: false, baseProtection: null,
  account: { host: 'github.com', login: 'dana-lee', version: '2.100.0' } });
const source = (role, provider, invocation) => ({ step: invocation, invocation, ordinal: 0, role, provider,
  model: provider === 'claude' ? 'sonnet' : null, workspace: 'snapshot', treeSha: 'aa11' });
const finding = (id, title, path, role, provider, invocation, extra = {}) => ({ id, flow: 9,
  class: { kind: 'suggestion' }, severity: 'notable', location: { path, startLine: 7, endLine: 7 },
  title, rationale: 'Why it matters.', requirement: null, treeSha: 'aa11', dismissed: null, lines: true,
  evidence: [{ source: source(role, provider, invocation), reported: { title, severity: 'notable', class: 'suggestion', body: 'x', path, line: 7 } }], ...extra });
const offByOne = finding(91, 'The export loop reads one row past the limit', 'src/export.py', 'correctness', 'codex', 1);
const flag = finding(92, 'The guide names a flag the CLI does not have', 'docs/guide.md', 'docs', 'claude', 2);
const step = (id, role, provider, n) => ({ id, stage: 0, role, provider, model: provider === 'claude' ? 'sonnet' : null,
  profile: 'reviewer', workspace: 'snapshot', status: 'done', failures: 0, retries: 1, standsInFor: null,
  detail: null, resetsAt: null, skillProblem: null,
  result: { kind: 'findings', payload: { findings: Array.from({ length: n }, () => ({ title: 't', severity: 'notable', class: 'suggestion', body: 'x' })) } } });
const outcome = { id: 1, flow: 9, kind: 'private', headSha: FIRST, notes: 'Ask Sam whether the limit is inclusive.', findings: [91, 92], decidedAt: 1 };
const view = () => {
  const p = window.phase;
  const rereviewed = p === 'rereviewed';
  const head = rereviewed ? SECOND : FIRST;
  return {
    flow: { id: 9, state: { kind: 'review_pr', phase: 'consolidated', hold: null, end: null }, startedAt: 0, updatedAt: 0 },
    review: { flow: 9, project: '/x/webapp', worktree: '/x/webapp-pr-4031', worktreePath: '/x/webapp-pr-4031', recipe: 'standard', base: 'origin/main', snapshot: rereviewed ? 2 : 1 },
    snapshot: { headSha: head, treeSha: 'aa11', baseSha: 'c0ffee11' },
    steps: [step(1, 'correctness', 'codex', rereviewed ? 0 : 1), step(2, 'docs', 'claude', 1)],
    invocations: [], inbox: [], running: false,
    findings: rereviewed ? [flag] : [offByOne, flag],
    checks: [], expectedChecks: [], applied: { rules: [], sources: ['CLAUDE.md'], skills: [] },
    pullRequest: {
      item: 1, repo: 'acme/webapp', number: 4031,
      mirror: { item: 1, pullRequest: pr(forgeHead()), permission: 'write', readAt: 1 },
      reviewing: item.reviewingSince === null ? null : { item: 1, since: item.reviewingSince },
      reviewedHead: head,
      movedTo: p === 'moved' ? SECOND : null,
      pushToBranch: 'fork_without_maintainer_edits',
      earlier: rereviewed ? [{
        head: { id: 1, flow: 9, headSha: FIRST, snapshot: 1, findings: [91, 92], checks: [], nextHead: SECOND, leftAt: 2 },
        findings: [{ ...offByOne, fate: 'not_reported_again' }, { ...flag, fate: 'unchanged' }],
        checks: [],
      }] : [],
      outcomes: ['kept', 'moved', 'rereviewed'].includes(p) ? [outcome] : [],
    },
  };
};
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
    case 'set_reviewing_pull_request': item.reviewingSince = a.on ? 5 : null; return structuredClone(item);
    case 'preview_review_worktree': return { number: 4031, head: forgeHead(), fetch: null, headPresent: true,
      target: { kind: 'reuse', worktreeId: '/x/webapp-pr-4031', path: '/x/webapp-pr-4031', reason: 'review_directory', atHead: true },
      setup: { kind: 'not_applicable' }, preflight: [], warnings: [], canCreate: true };
    case 'review_recipes': return [{ id: 'standard', label: 'Standard', roles: [
      { id: 'correctness', provider: 'codex', model: null, skills: [] }, { id: 'docs', provider: 'claude', model: 'sonnet', skills: [] }] }];
    case 'latest_review': return a.worktree === '/x/webapp-pr-4031' && window.phase !== 'none' ? view() : null;
    case 'review_run': return view();
    case 'review_staleness': {
      const moved = window.phase === 'moved';
      return { flow: 9, treeSha: 'aa11', headSha: FIRST, changed: moved, paths: [], complete: !moved,
        staleFindings: moved ? [91, 92] : [], staleChecks: [], affected: [],
        pullRequestHead: moved ? SECOND : null, pullRequestHeadHere: !moved };
    }
    case 'start_pull_request_review': window.phase = 'reviewed'; return view();
    case 'keep_review_private': window.phase = 'kept'; return { ...outcome, notes: a.notes };
    case 'rereview_pull_request': window.phase = 'rereviewed'; return view();
    case 'work_inbox': return [];
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
window.harness = { emit, view: views, workspace, work, FIRST, SECOND };
mount(App, { target: document.getElementById('app') });`;

const server = await createServer({
    configFile: false,
    envDir: false,
    logLevel: 'warn',
    plugins: [
        svelte(),
        {
            name: 'pr-review-probe',
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
    const state = (page) => panel(page).locator('.c-work-state').first();
    async function eyebrow(page, words) {
        await state(page).locator('.c-work-state__eyebrow', { hasText: words }).waitFor();
        return state(page).innerText();
    }

    for (const scheme of ['light', 'dark']) {
        const context = await browser.newContext({
            viewport: { width: 1280, height: 900 },
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
        const { FIRST, SECOND } = await page.evaluate(() => ({
            FIRST: window.harness.FIRST,
            SECOND: window.harness.SECOND,
        }));

        // ── A pull request with a review worktree opens on Reviews ────────────────────────
        await queue(page).getByRole('button', { name: /#4031/ }).click();
        await page.getByRole('tab', { name: 'Reviews', selected: true }).waitFor();
        let text = await eyebrow(page, 'Not reviewed yet');
        assert(text.includes('Review #4031'), text);
        assert(text.includes('against acme/webapp main, where it would merge'), text);
        if (scheme === 'light')
            await shot(page, join(shots, `pr-not-reviewed-${scheme}.png`));

        await panel(page).getByRole('button', { name: 'Run Review' }).click();
        assert.deepEqual((await calls(page, 'start_pull_request_review')).at(-1).args, {
            project: '/x/webapp',
            item: 1,
            recipe: 'standard',
            expectedHead: FIRST,
        });

        // ── Reviewed: which head was read, and that the pull request is still there ─────────
        text = await eyebrow(page, 'Reviewed');
        const heads = await panel(page).locator('.c-review__heads').innerText();
        assert(heads.includes(`Reviewed head ${FIRST.slice(0, 7)}`), heads);
        assert(heads.includes(`Current head ${FIRST.slice(0, 7)} ✓`), heads);
        const meta = await panel(page).locator('.c-review__meta').first().innerText();
        assert(meta.includes('Compared with origin/main, where #4031 would merge'), meta);
        await shot(page, join(shots, `pr-reviewed-${scheme}.png`));

        // ── The outcome sheet: private works, the rest come next, a fork is not pushed to ───
        await panel(page).getByRole('button', { name: 'Finish review…' }).click();
        const sheet = page.getByRole('dialog', { name: 'Finish your review of #4031' });
        await sheet.waitFor();
        const sheetText = await sheet.innerText();
        for (const words of [
            'sam-rivera:feature/csv-export → acme/webapp main',
            'Fork · maintainer edits off',
            'Keep private notes',
            'Nothing is posted to GitHub or Jira.',
            'Unavailable: fork without maintainer edits',
            '2 findings stay beside the notes',
        ])
            assert(sheetText.includes(words), `the sheet says “${words}”:\n${sheetText}`);
        assert.equal(
            await sheet.getByText('Coming next').count(),
            2,
            'Publish and Local fixes both say they come next',
        );
        const radios = sheet.getByRole('radio');
        assert.equal(await radios.count(), 3);
        assert.equal(await radios.nth(0).isChecked(), true);
        assert.equal(await radios.nth(1).isDisabled(), true, 'Publish is not offered yet');
        assert.equal(
            await radios.nth(2).isDisabled(),
            true,
            'Local fixes are not offered yet',
        );
        const push = sheet.locator('.c-pr-outcome__destination.is-unavailable');
        assert.match(await push.innerText(), /Push to sam-rivera:feature\/csv-export/);
        await sheet
            .getByRole('textbox', { name: 'Notes for yourself' })
            .fill('Ask Sam whether the limit is inclusive.');
        await shot(page, join(shots, `pr-outcome-sheet-${scheme}.png`));
        await sheet.getByRole('button', { name: 'Keep private notes' }).click();
        await sheet.waitFor({ state: 'detached' });
        assert.deepEqual((await calls(page, 'keep_review_private')).at(-1).args, {
            flow: 9,
            notes: 'Ask Sam whether the limit is inclusive.',
        });
        await state(page)
            .getByText(
                `Kept private at ${FIRST.slice(0, 7)} · 2 findings · nothing was posted`,
            )
            .waitFor();

        // ── The author pushes: the next reading makes the review stale ──────────────────────
        await page.evaluate(() => (window.phase = 'moved'));
        await page.getByRole('button', { name: 'Refresh' }).click();
        text = await eyebrow(page, 'Stale');
        assert(text.includes('New commits on #4031 since this review'), text);
        assert(text.includes('wtm has not fetched the new commits'), text);
        assert.equal(
            await panel(page)
                .getByRole('button', { name: /Rerun affected/ })
                .count(),
            0,
            'Rerun affected would read the old head again',
        );
        assert.equal(await panel(page).locator('.c-finding__stale').count(), 2);
        await page
            .locator('.c-work-row', { hasText: '#4031' })
            .getByText(/review stale/)
            .waitFor();
        await shot(page, join(shots, `pr-moved-${scheme}.png`));

        await panel(page).getByRole('button', { name: 'Re-review changes' }).click();
        assert.deepEqual((await calls(page, 'rereview_pull_request')).at(-1).args, {
            project: '/x/webapp',
            flow: 9,
            expectedHead: SECOND,
        });

        // ── The head it left, and what became of each finding ───────────────────────────────
        await eyebrow(page, 'Reviewed');
        const earlier = panel(page).locator('.c-review-earlier');
        await earlier.locator('summary').click();
        const earlierText = await earlier.innerText();
        assert(
            earlierText.includes(`${FIRST.slice(0, 7)}, until the author pushed`),
            earlierText,
        );
        assert(earlierText.includes('Not reported again'), earlierText);
        assert(earlierText.includes('Still open: its files did not change'), earlierText);
        assert(earlierText.includes('Kept private here'), earlierText);
        await shot(page, join(shots, `pr-rereviewed-${scheme}.png`));

        // ── I'm reviewing this: sent, and shown in the queue ─────────────────────────────────
        await page.getByRole('checkbox', { name: "I'm reviewing this" }).check();
        assert.deepEqual((await calls(page, 'set_reviewing_pull_request')).at(-1).args, {
            projectId: '/x/webapp',
            itemId: 1,
            on: true,
        });
        await page
            .locator('.c-work-row', { hasText: '#4031' })
            .getByText("you're reviewing")
            .waitFor();
        await page.getByRole('checkbox', { name: "I'm reviewing this" }).uncheck();
        await page.waitForFunction(
            () =>
                !document
                    .querySelector('.c-work-queue')
                    ?.textContent?.includes("you're reviewing"),
        );

        // ── Overview still has the forge's facts and the Prepare panel ──────────────────────
        await page.getByRole('tab', { name: 'Overview' }).click();
        await page
            .getByRole('tabpanel', { name: 'Overview' })
            .getByText('Your permission')
            .waitFor();
        await context.close();
    }

    console.log(`ok — screenshots in ${shots}`);
} finally {
    await browser?.close();
    await server.close();
}

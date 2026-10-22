/**
 * Drive the real app shell with mocked IPC: the Work view's sprint queue, its board and an issue's
 * workspace. No Tauri, no `acli`, no network.
 *
 * What it proves, in order: a project whose workflow names no tracker keeps the review slice's
 * queue and asks for no sync; one that names a tracker syncs once when Work opens, shows the
 * sprint's name and goal, and lists its issues in Needs you, Running, Ready to test, Reviews and
 * Sprint, not started, with the team status as a chip beside the run's own words, and the worktree
 * that carries an issue's key shown through that issue rather than in Other worktrees; the board's
 * columns are the mapped statuses, with an unmapped status in a column that names it; an issue
 * moved in the tracker moves on the board after Sync and starts nothing (no review or flow
 * command is sent); a focus straight after a sync asks the tracker nothing; and a card opens the
 * issue, read from the mirror at once and from the tracker after.
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
const shots = process.env.WTM_SHOTS || join(tmpdir(), 'wtm-sprint-queue');
mkdirSync(shots, { recursive: true });

/*
 * The page: the real `App` with the Work view on, after a mock of every command it calls. The
 * mocked backend answers the tracker commands the way `tracker.rs` does: `tracker_queue` reads the
 * mirror, `sync_tracker` reads "the tracker" into it, and `window.moveInTracker()` moves an issue
 * there the way a teammate would, unseen until the next sync. `?tracker=none` is a project whose
 * workflow names no tracker.
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
const project = { id: '/x/webapp', name: 'webapp', root: '/x/webapp', usable: true, problem: null, trust: null };
const wt = (dir, branch, extra = {}) => ({
  id: dir === 'webapp' ? '/x/webapp' : '/x/' + dir, title: dir, subtitle: branch ?? '(detached)',
  path: dir === 'webapp' ? '/x/webapp' : '/x/' + dir, dirname: dir, branch, head: 'a1b2c3d4e5',
  isMain: dir === 'webapp', isBare: false, locked: null, prunable: null, dirty: false, untracked: 0,
  staged: 0, ahead: 0, behind: 0, issueKey: null, badges: [], links: [], table: [], env: [],
  browserHome: null, ...extra });
const worktrees = [
  wt('webapp', 'main'),
  wt('webapp-acme-102', 'feature/ACME-102-export', { issueKey: 'ACME-102' }),
  wt('fix-date-picker', 'fix/date-picker'),
];
const tracker = params.get('tracker') === 'none' ? null : 'jira';
const status = (name, category) => ({ name, category });
const issue = (key, summary, s, extra = {}) => ({
  key, summary, status: s, kind: 'Story', priority: 'Medium', assignee: null, labels: [], ...extra });
const tracked = [
  issue('ACME-101', 'Bulk unit export', status('To Do', 'to_do')),
  issue('ACME-102', 'Export units as CSV', status('In Progress', 'in_progress'), { assignee: 'Sam Rivera' }),
  issue('ACME-103', 'Role sidebar', status('In Review', 'in_progress')),
  issue('ACME-104', 'Date picker keeps the old value', status('Done', 'done')),
  issue('ACME-105', 'Import from the old format', status('Triage', 'to_do')),
];
const columns = [
  { role: 'todo', status: 'To Do' }, { role: 'started', status: 'In Progress' },
  { role: 'needs_review', status: 'In Review' }, { role: 'done', status: 'Done' },
];
const column = (name) => { const i = columns.findIndex((c) => c.status === name); return i < 0 ? null : i; };
const role = (name) => columns.find((c) => c.status === name)?.role ?? null;
const sprint = { id: 41, name: 'Sprint 41', state: 'active', goal: 'Ship the bulk export',
  startsAt: '2026-10-12T14:00:00.000Z', endsAt: '2026-10-23T22:00:00.000Z', board: 3 };
const waiting = { flow: 7, worktree: '/x/webapp-acme-102',
  state: { kind: 'review_mine', phase: 'reviewing', hold: { kind: 'awaiting_answer' }, end: null }, openItems: 1 };
let mirrored = null;
let readAt = null;
const view = () => ({
  tracker, query: tracker ? 'assignee = currentUser()' : null, board: tracker ? 3 : null,
  columns: tracker ? columns : [], sprints: mirrored ? [sprint] : [],
  issues: (mirrored ?? []).map((i) => ({
    tracker: 'jira', issue: structuredClone(i), sprint: 41, column: column(i.status.name),
    role: role(i.status.name), workItem: null,
    worktrees: i.key === 'ACME-102' ? ['/x/webapp-acme-102'] : [],
    run: i.key === 'ACME-102' ? waiting : null,
    section: i.key === 'ACME-102' ? 'needs_you' : 'not_started' })),
  readAt, failure: null, outdated: false });
window.moveInTracker = (key, name, category) => {
  tracked.find((i) => i.key === key).status = status(name, category);
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
    case 'review_queue': return { forge: null, items: [] };
    case 'tracker_queue': return view();
    case 'sync_tracker': {
      if (!tracker) throw { kind: 'trackerOff', message: 'no tracker', detail: null };
      mirrored = structuredClone(tracked);
      readAt = Date.now();
      return { queue: view(), report: { reason: a.reason, account: { site: 'acme.atlassian.net', login: 'you@example.com', version: '1.3.18' },
        issues: tracked.length, sprints: [sprint], changes: { moved: [], joined: [], left: [] }, readAt } };
    }
    case 'tracker_issue': {
      const found = (mirrored ?? []).find((i) => i.key === a.key);
      if (!found || (!a.fresh && !window.keptOnce)) return null;
      return { tracker: 'jira', readAt: Date.now(), column: column(found.status.name), role: role(found.status.name),
        account: a.fresh ? { site: 'acme.atlassian.net', login: 'you@example.com', version: '1.3.18' } : null,
        detail: { issue: structuredClone(found), description: 'Export every unit.\\n- as CSV\\n- with a header row',
          parent: { key: 'ACME-90', summary: 'Exports' }, updated: null, sprints: [sprint],
          links: [{ relation: 'is blocked by', key: 'OPS-7', summary: 'Storage quota', status: status('Done', 'done') }],
          fields: [{ name: 'owner', id: 'customfield_10050', value: 'Priya Shah' }] } };
    }
    default: return cmd.startsWith('list_') ? [] : null;
  }
}, { shouldMockEvents: true });
await import('@xterm/xterm/css/xterm.css');
await import('/src/styles/main.scss');
const { mount } = await import('svelte');
const { default: App } = await import('/src/App.svelte');
const { view: views } = await import('/src/lib/state/view.svelte.ts');
const { workspace } = await import('/src/lib/state/workspace.svelte.ts');
window.harness = { emit, view: views, workspace };
mount(App, { target: document.getElementById('app') });`;

const server = await createServer({
    configFile: false,
    envDir: false,
    logLevel: 'warn',
    plugins: [
        svelte(),
        {
            name: 'sprint-queue-probe',
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

    async function open(query = '', colorScheme = 'light') {
        const context = await browser.newContext({
            viewport: { width: 1280, height: 860 },
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
        return page;
    }

    const shot = (page, path) => page.screenshot({ path, animations: 'disabled' });
    const calls = (page, cmd) =>
        page.evaluate((c) => window.calls.filter((x) => x.cmd === c), cmd);
    const queue = (page) => page.getByRole('navigation', { name: 'Work queue' });
    const headings = (page) =>
        queue(page)
            .locator('.c-work-queue__heading')
            .evaluateAll((els) => els.map((el) => el.firstChild.textContent.trim()));
    const section = (page, name) =>
        queue(page).getByRole('region', { name: new RegExp(`^${name}`) });
    const board = (page) => page.getByRole('region', { name: 'Board', exact: true });
    const boardColumn = (page, name) =>
        board(page).getByRole('region', { name: new RegExp(`^${name} \\d+$`) });

    // ── A project with no tracker: the review slice's queue, and no sync ─────────────────────
    {
        const page = await open('?tracker=none');
        await section(page, 'My worktrees').waitFor();
        assert.deepEqual(await headings(page), ['My worktrees', 'Reviews']);
        assert.equal((await calls(page, 'sync_tracker')).length, 0);
        assert.equal(await page.getByRole('region', { name: 'Sprint' }).count(), 0);
        await page.context().close();
    }

    for (const scheme of ['light', 'dark']) {
        const page = await open(`?theme=${scheme}`, scheme);

        // ── Opening Work syncs once, and the sprint sections fill ──────────────────────────
        await page.getByRole('region', { name: 'Sprint' }).getByText('Sprint 41').waitFor();
        await page.getByText('Goal: Ship the bulk export').first().waitFor();
        const syncs = await calls(page, 'sync_tracker');
        assert.deepEqual(
            syncs.map((c) => c.args),
            [{ projectId: '/x/webapp', reason: 'open' }],
        );
        assert.deepEqual(await headings(page), [
            'Needs you',
            'Running',
            'Ready to test',
            'Reviews',
            'Sprint, not started',
            'Other worktrees',
        ]);
        const needsYou = section(page, 'Needs you').locator('.c-work-row');
        assert.equal(await needsYou.count(), 1);
        const waitingRow = await needsYou.first().innerText();
        assert.match(waitingRow, /ACME-102 · Export units as CSV/);
        // The team status and the run status, side by side, each in its own place.
        assert.equal(
            await needsYou.first().locator('.c-team-status').innerText(),
            'In Progress',
        );
        assert.match(waitingRow, /Run waiting on your answer/);
        assert.equal(
            await section(page, 'Sprint, not started').locator('.c-work-row').count(),
            4,
        );
        // The worktree carrying ACME-102 is that issue's, so Other worktrees leaves it out.
        const others = await section(page, 'Other worktrees').innerText();
        assert(!others.includes('ACME-102'), others);
        assert.match(others, /date-picker/);
        await shot(page, join(shots, `queue-${scheme}.png`));

        // ── The board: mapped statuses as columns, an unmapped one named ───────────────────
        await queue(page).getByRole('button', { name: 'Board' }).click();
        await board(page).waitFor();
        const titles = await board(page)
            .locator('.c-board__column-title')
            .evaluateAll((els) => els.map((el) => el.firstChild.textContent.trim()));
        assert.deepEqual(titles, [
            'To Do',
            'In Progress',
            'In Review',
            'Done',
            'Not mapped',
        ]);
        await boardColumn(page, 'Not mapped').getByText('Triage').first().waitFor();
        await boardColumn(page, 'To Do').getByText('ACME-101').waitFor();
        await shot(page, join(shots, `board-${scheme}.png`));

        // ── Moved in the tracker: Sync moves it on the board, and nothing starts ───────────
        await page.evaluate(() =>
            window.moveInTracker('ACME-101', 'In Review', 'in_progress'),
        );
        await queue(page).getByRole('button', { name: 'Sync' }).click();
        await boardColumn(page, 'In Review').getByText('ACME-101').waitFor();
        assert.equal(await boardColumn(page, 'To Do').locator('.c-board-card').count(), 0);
        assert.deepEqual(
            (await calls(page, 'sync_tracker')).map((c) => c.args.reason),
            ['open', 'demand'],
        );
        const started = await page.evaluate(() =>
            window.calls.filter((c) => /start|rerun|resume|fix|publish/.test(c.cmd)),
        );
        assert.deepEqual(started, [], 'a status change started something');

        // ── Focus right after a sync asks the tracker nothing ──────────────────────────────
        await page.evaluate(() => window.dispatchEvent(new Event('focus')));
        await page.waitForTimeout(150);
        assert.equal((await calls(page, 'sync_tracker')).length, 2);

        // ── A card opens its issue: the mirror at once, the tracker after ──────────────────
        await page.evaluate(() => (window.keptOnce = true));
        await boardColumn(page, 'In Progress')
            .getByRole('button', { name: /ACME-102/ })
            .click();
        await page.locator('.c-work__title').filter({ hasText: 'ACME-102' }).waitFor();
        await page.getByRole('tabpanel').getByText('Export every unit.').waitFor();
        const reads = (await calls(page, 'tracker_issue')).map((c) => c.args.fresh);
        assert.deepEqual(reads.slice(0, 2), [false, true]);
        const header = page.locator('.c-work__header');
        assert.equal(await header.locator('.c-team-status').innerText(), 'In Progress');
        assert.match(await header.innerText(), /Run waiting on your answer/);
        const overview = await page.getByRole('tabpanel').innerText();
        for (const fact of [
            'is blocked by',
            'OPS-7',
            'ACME-90',
            'Priya Shah',
            'Start column',
        ])
            assert(overview.includes(fact), `the overview says “${fact}”:\n${overview}`);
        await shot(page, join(shots, `issue-${scheme}.png`));

        await page.context().close();
    }

    console.log(`ok — screenshots in ${shots}`);
} finally {
    await browser?.close();
    await server.close();
}

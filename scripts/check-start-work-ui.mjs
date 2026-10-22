/**
 * Drive the real app shell with mocked IPC: Start Work from the issue workspace, the queue's ⌘↵
 * and the board, and what the inbox offers about an issue's run. No Tauri, no `acli`, no git.
 *
 * What it proves, in order: an issue's workspace offers Start with what it does to the team
 * status written under it, and Start shows the run "Ready to plan" with its worktree and its move,
 * the race a tracker leaves open said beside it; ⌘↵ on the queue's row is the same Start, and on an
 * issue whose run exists it focuses that run and asks for nothing new; more than one place for the
 * work opens the Start sheet in the plan's order, nothing started until one is picked; on the
 * board only the start column says "Start work", a card dropped there is a Start, a card dragged
 * backward is a move and nothing else, and the inbox then offers to pause its run; and a card
 * dragged out of review asks before anything moves; and a setup that is running shows its terminal,
 * with what it printed before the workspace opened it.
 *
 * The mocked backend keeps what Rust would: one run per issue however often it is started, and a
 * count of worktrees made, which the harness reads to show a repeat Start makes none.
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
const shots = process.env.WTM_SHOTS || join(tmpdir(), 'wtm-start-work');
mkdirSync(shots, { recursive: true });

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
const worktrees = [wt('webapp', 'main')];
const status = (name, category) => ({ name, category });
const issue = (key, summary, s) => ({
  key, summary, status: s, kind: 'Story', priority: 'Medium', assignee: null, labels: [] });
const tracked = [
  issue('ACME-101', 'Bulk unit export', status('To Do', 'to_do')),
  issue('ACME-103', 'Role sidebar', status('In Review', 'in_progress')),
  issue('ACME-105', 'Import from the old format', status('To Do', 'to_do')),
  issue('ACME-106', 'Audit log filters', status('To Do', 'to_do')),
];
const columns = [
  { role: 'todo', status: 'To Do' }, { role: 'started', status: 'In Progress' },
  { role: 'started', status: 'Blocked' }, { role: 'needs_review', status: 'In Review' },
  { role: 'done', status: 'Done' },
];
const column = (name) => { const i = columns.findIndex((c) => c.status === name); return i < 0 ? null : i; };
const role = (name) => columns.find((c) => c.status === name)?.role ?? null;
const sprint = { id: 41, name: 'Sprint 41', state: 'active', goal: 'Ship the bulk export',
  startsAt: null, endsAt: null, board: 3 };

/* Rust's record, kept as the engine keeps it: one run per issue, its job, its moves, the inbox. */
const runs = {};
const inbox = [];
let made = 0;
let nextFlow = 20;
let nextItem = 1;
const flowOf = (key) => runs[key]?.flow ?? null;
const runView = (key) => {
  const run = runs[key];
  if (!run) return null;
  return { flow: run.flow.id, worktree: run.job.worktree, state: structuredClone(run.flow.state),
    openItems: inbox.filter((i) => i.flow === run.flow.id).length, start: run.job.state };
};
const view = () => ({
  tracker: 'jira', query: 'assignee = currentUser()', board: 3, columns, sprints: [sprint],
  issues: tracked.map((i) => ({
    tracker: 'jira', issue: structuredClone(i), sprint: 41, column: column(i.status.name),
    role: role(i.status.name), workItem: runs[i.key]?.flow.workItem ?? null,
    worktrees: runs[i.key] ? [runs[i.key].job.worktree] : [], run: runView(i.key),
    section: runs[i.key] ? (inbox.some((x) => x.flow === runs[i.key].flow.id) ? 'needs_you' : 'running') : 'not_started' })),
  readAt: Date.now(), failure: null, outdated: false });
const startMoveOf = (key, byDrop) => {
  const s = tracked.find((i) => i.key === key).status.name;
  if (s === 'In Progress') return { kind: 'stay', reason: 'already_there' };
  if (!byDrop && role(s) !== 'todo') return { kind: 'stay', reason: 'not_to_do' };
  return { kind: 'move', from: s, to: 'In Progress', leavingReview: role(s) === 'needs_review' };
};
const moveTo = (key, to, cause, from) => {
  const found = tracked.find((i) => i.key === key);
  found.status = status(to, to === 'To Do' ? 'to_do' : to === 'Done' ? 'done' : 'in_progress');
  const id = 100 + runs[key].moves.length;
  runs[key].moves.push({ approval: id, request: { payload: { tracker: 'jira', key, from, to }, cause },
    operation: { id, key: { flow: runs[key].flow.id, kind: 'move_issue', target: 'jira:' + key, payloadHash: 'h' + id },
      approval: id, position: 0, payload: { kind: 'move_issue', tracker: 'jira', key, from, to }, state: 'done',
      attempts: 1, lastError: null, result: { kind: 'moved', key, status: to, adopted: false }, attention: null,
      resolution: null, createdAt: Date.now(), updatedAt: Date.now() }, state: 'published' });
};
const file = (key, title, body) => {
  const item = { id: nextItem++, flow: runs[key].flow.id, kind: 'approval', target: { kind: 'flow' }, title, body,
    state: 'open', createdAt: Date.now(), closedAt: null, project: '/x/webapp', worktree: runs[key].job.worktree,
    issue: key, question: null };
  inbox.push(item);
  setTimeout(() => emit('flow:inbox', item), 0);
};
const candidates = (key) => key === 'ACME-105'
  ? [{ kind: 'key_in_name', worktree: { id: '/x/acme-105-a', branch: 'feature/ACME-105-a', key } },
     { kind: 'key_in_name', worktree: { id: '/x/acme-105-b', branch: 'bugfix/ACME-105-b', key } },
     { kind: 'new', branch: 'feature/' + key, directory: '/x/' + key, base: 'main' }]
  : [{ kind: 'new', branch: 'feature/' + key, directory: '/x/' + key, base: 'main' }];
const idOf = (c) => c.worktree ? 'worktree:' + c.worktree.id : c.kind === 'new' ? 'new' : 'branch:' + c.branch;
const start = (req) => {
  const key = req.key;
  if (runs[key]) return { kind: 'focused', flow: runs[key].flow.id };
  const found = candidates(key);
  if (found.length > 1 && !req.choice)
    return { kind: 'choose', preview: { key, status: tracked.find((i) => i.key === key).status.name,
      resolved: { kind: 'choose', candidates: found }, problems: [], startMove: startMoveOf(key, req.byDrop), open: null } };
  const move = startMoveOf(key, req.byDrop);
  if (move.kind === 'move' && move.leavingReview && !req.confirmed)
    return { kind: 'confirm', from: move.from, to: move.to };
  const candidate = req.choice ? found.find((c) => idOf(c) === req.choice) : found[0];
  if (candidate.kind === 'new') {
    made += 1;
    worktrees.push(wt(key, candidate.branch));
  }
  const id = nextFlow++;
  runs[key] = { flow: { id, workItem: id, state: { kind: 'implement', phase: 'preparing', hold: null, end: null }, seq: 2,
      startedAt: Date.now(), updatedAt: Date.now() },
    job: { workItem: id, flow: id, project: '/x/webapp', key, candidate,
      worktree: candidate.worktree?.id ?? candidate.directory, branch: candidate.worktree?.branch ?? candidate.branch,
      state: 'ready', detail: null, setupSession: null, createdAt: Date.now(), updatedAt: Date.now() },
    moves: [] };
  if (move.kind === 'move') moveTo(key, move.to, 'start', move.from);
  setTimeout(() => emit('flow:changed', { flow: id }), 0);
  return { kind: 'started', flow: id, candidate };
};
window.made = () => made;
/* A start whose setup is running now, in a terminal the app kept output for. */
window.settingUp = (key) => {
  runs[key].job.state = 'setting_up';
  runs[key].job.setupSession = 'pty-7';
  emit('flow:changed', { flow: runs[key].flow.id });
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
    case 'sync_tracker': return { queue: view(), report: null };
    case 'tracker_issue': {
      const found = tracked.find((i) => i.key === a.key);
      return { tracker: 'jira', readAt: Date.now(), column: column(found.status.name), role: role(found.status.name),
        account: null, detail: { issue: structuredClone(found), description: 'What the issue asks for.', parent: null,
          updated: null, sprints: [sprint], links: [], fields: [] } };
    }
    case 'issue_start': {
      const run = runs[a.key];
      return { workItem: run?.flow.workItem ?? null, flow: run ? structuredClone(run.flow) : null,
        job: run ? structuredClone(run.job) : null, working: false, moves: run ? structuredClone(run.moves) : [],
        inbox: inbox.filter((i) => i.flow === flowOf(a.key)?.id), startMove: startMoveOf(a.key, false),
        startStatus: 'In Progress' };
    }
    case 'start_work': return start(a.request);
    case 'move_issue': {
      const found = tracked.find((i) => i.key === a.key);
      const from = found.status.name;
      if (a.to === 'In Progress') return { kind: 'start' };
      if (from === a.to) return { kind: 'stayed' };
      if (role(from) === 'needs_review' && !a.confirmed) return { kind: 'confirm', from, to: a.to };
      if (!runs[a.key]) throw { kind: 'flow', message: 'wtm moves ' + a.key + ' only once you have started it here', detail: null };
      moveTo(a.key, a.to, 'drop', from);
      if (column(a.to) < column(from)) file(a.key, a.key + ' moved back to ' + a.to + '. Pause its run?',
        'Moving an issue backward changes only its team status: its run and its worktree are as they were.');
      setTimeout(() => emit('flow:changed', { flow: runs[a.key].flow.id }), 0);
      return { kind: 'moving', flow: runs[a.key].flow.id };
    }
    case 'pause_run': {
      const key = Object.keys(runs).find((k) => runs[k].flow.id === a.flow);
      runs[key].flow.state.hold = { kind: 'paused' };
      inbox.splice(0, inbox.length, ...inbox.filter((i) => i.flow !== a.flow));
      setTimeout(() => emit('flow:changed', { flow: a.flow }), 0);
      return null;
    }
    case 'work_inbox': return structuredClone(inbox);
    case 'terminal_replay':
      return a.session === 'pty-7' ? { through: 2, chunkBase64: btoa('step 1\\r\\nstep 2\\r\\n') } : null;
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
            name: 'start-work-probe',
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
        await queue(page).getByText('ACME-101').waitFor();
        return page;
    }

    const shot = (page, path) => page.screenshot({ path, animations: 'disabled' });
    const calls = (page, cmd) =>
        page.evaluate((c) => window.calls.filter((x) => x.cmd === c), cmd);
    const queue = (page) => page.getByRole('navigation', { name: 'Work queue' });
    const row = (page, key) => queue(page).locator('.c-work-row').filter({ hasText: key });
    const header = (page) => page.locator('.c-work__header');
    const runSection = (page) => page.getByRole('region', { name: 'Run', exact: true });
    const board = (page) => page.getByRole('region', { name: 'Board', exact: true });
    const boardColumn = (page, name) =>
        board(page).getByRole('region', { name: new RegExp(`^${name} \\d+$`) });

    /** Drag a board card onto a column with the pointer, as a person would. */
    async function drag(page, key, to) {
        const card = board(page).getByRole('button', { name: new RegExp(key) });
        const from = await card.boundingBox();
        const target = await boardColumn(page, to).boundingBox();
        await page.mouse.move(from.x + from.width / 2, from.y + from.height / 2);
        await page.mouse.down();
        await page.mouse.move(target.x + target.width / 2, target.y + 80, { steps: 12 });
        return {
            drop: () => page.mouse.up(),
        };
    }

    for (const scheme of ['light', 'dark']) {
        const page = await open(`?theme=${scheme}`, scheme);

        // ── The workspace offers Start, saying what it does to the team status ─────────────
        await row(page, 'ACME-101').click();
        const start = header(page).getByRole('button', { name: 'Start work' });
        await start.waitFor();
        assert.match(await header(page).innerText(), /Moves it to In Progress in Jira/);
        await shot(page, join(shots, `issue-before-start-${scheme}.png`));

        // ── Start: a run, ready to plan, its worktree and its move, the race said ──────────
        await start.click();
        await runSection(page).getByText('Ready to plan').waitFor();
        const run = await runSection(page).innerText();
        for (const fact of [
            'feature/ACME-101',
            'Moved from To Do to In Progress',
            'cannot check a status and move an issue in one step',
        ])
            assert(run.includes(fact), `the run says “${fact}”:\n${run}`);
        // The move is the header's word too, read back from the mirror without asking the tracker,
        // and the worktree Start made is in the window's list.
        await header(page).locator('.c-team-status').getByText('In Progress').waitFor();
        await page
            .getByRole('tabpanel')
            .getByRole('button', { name: 'Open' })
            .first()
            .waitFor();
        assert.deepEqual(
            (await calls(page, 'start_work')).map((c) => c.args.request),
            [
                {
                    project: '/x/webapp',
                    key: 'ACME-101',
                    choice: null,
                    byDrop: false,
                    confirmed: false,
                },
            ],
        );
        await header(page).getByRole('button', { name: 'Go to run' }).waitFor();
        assert.equal(await page.evaluate(() => window.made()), 1);
        await shot(page, join(shots, `issue-ready-to-plan-${scheme}.png`));

        // ── ⌘↵ on its row again: the same run, focused, and no second worktree ─────────────
        await row(page, 'ACME-101').focus();
        await page.keyboard.press('Meta+Enter');
        await page.waitForFunction(
            () => window.calls.filter((c) => c.cmd === 'start_work').length === 2,
        );
        assert.equal(
            await page.evaluate(() => window.made()),
            1,
            'a repeat Start made nothing',
        );
        assert.equal(await page.evaluate(() => window.harness.work.issue), 'ACME-101');
        await runSection(page).getByText('Ready to plan').waitFor();

        // ── More than one place: the sheet, in order, nothing started until one is picked ──
        await row(page, 'ACME-105').focus();
        await page.keyboard.press('Meta+Enter');
        const sheet = page.getByRole('dialog', {
            name: "Where should ACME-105's work happen?",
        });
        await sheet.waitFor();
        const radios = sheet.getByRole('radio');
        assert.equal(await radios.count(), 3);
        assert.equal(await radios.first().isChecked(), true, 'the plan’s first is picked');
        const choices = await sheet.locator('.c-start-sheet__choice').allInnerTexts();
        assert.match(choices[0], /feature\/ACME-105-a/);
        assert.match(choices[2], /A new worktree from main/);
        assert.match(
            await sheet.innerText(),
            /Start also moves ACME-105 from To Do to In Progress/,
        );
        assert.equal(await page.evaluate(() => window.made()), 1, 'nothing made by asking');
        await shot(page, join(shots, `start-sheet-${scheme}.png`));
        await radios.nth(1).check();
        await sheet.getByRole('button', { name: 'Start', exact: true }).click();
        await sheet.waitFor({ state: 'detached' });
        const picked = (await calls(page, 'start_work')).at(-1).args.request;
        assert.equal(picked.choice, 'worktree:/x/acme-105-b');
        await runSection(page).getByText('bugfix/ACME-105-b').waitFor();
        assert.equal(
            await page.evaluate(() => window.made()),
            1,
            'an existing worktree was adopted',
        );

        // ── The board: only the start column says "Start work" ─────────────────────────────
        await queue(page).getByRole('button', { name: 'Board' }).click();
        await board(page).waitFor();
        assert.equal(await board(page).locator('.c-board__start').count(), 1);
        assert.match(
            await boardColumn(page, 'In Progress').innerText(),
            /Start work · drop a card here to start it/,
        );
        assert.doesNotMatch(await boardColumn(page, 'Blocked').innerText(), /Start work/);
        await shot(page, join(shots, `board-${scheme}.png`));

        // ── A card dropped on the start column is a Start ──────────────────────────────────
        {
            const held = await drag(page, 'ACME-106', 'In Progress');
            await boardColumn(page, 'In Progress')
                .locator('.c-board__hint')
                .getByText('Start work', { exact: true })
                .waitFor();
            await shot(page, join(shots, `board-drag-to-start-${scheme}.png`));
            await held.drop();
        }
        await page.waitForFunction(() =>
            window.calls.some(
                (c) => c.cmd === 'start_work' && c.args.request.key === 'ACME-106',
            ),
        );
        const dropped = (await calls(page, 'start_work')).at(-1).args.request;
        assert.equal(dropped.byDrop, true);
        await runSection(page).getByText('Ready to plan').waitFor();
        assert.equal(await page.evaluate(() => window.made()), 2);

        // ── Backward: the status moves, the run is left alone, the inbox offers a pause ────
        await queue(page).getByRole('button', { name: 'Board' }).click();
        await board(page).waitFor();
        const before = await page.evaluate(() => window.calls.length);
        {
            const held = await drag(page, 'ACME-101', 'To Do');
            await boardColumn(page, 'To Do')
                .getByText('Move back to To Do: its run is left alone')
                .waitFor();
            await shot(page, join(shots, `board-drag-back-${scheme}.png`));
            await held.drop();
        }
        await boardColumn(page, 'To Do').getByText('ACME-101').waitFor();
        const after = await page.evaluate(
            (n) => window.calls.slice(n).map((c) => c.cmd),
            before,
        );
        assert(after.includes('move_issue'), after.join(', '));
        for (const touched of [
            'start_work',
            'cancel_issue_run',
            'pause_run',
            'remove_worktree',
        ])
            assert(!after.includes(touched), `a backward drag called ${touched}`);
        const offer = page
            .getByRole('complementary', { name: /Inbox/ })
            .locator('.c-run-inbox__item')
            .filter({ hasText: 'Pause its run?' });
        await offer.waitFor();
        await offer.getByRole('button').first().click();
        const card = page.locator('.c-inbox-card');
        await card.getByRole('button', { name: 'Pause the run' }).waitFor();
        await shot(page, join(shots, `inbox-pause-offer-${scheme}.png`));
        await card.getByRole('button', { name: 'Pause the run' }).click();
        await page.waitForFunction(() => window.calls.some((c) => c.cmd === 'pause_run'));

        // ── Out of review: asked first, and nothing moves until it is answered ─────────────
        await queue(page).getByRole('button', { name: 'Board' }).click();
        await board(page).waitFor();
        const moves = (await calls(page, 'move_issue')).length;
        // In Review is past the window's edge: held near the right edge, the board scrolls to it.
        {
            const columns = board(page).locator('.c-board__columns');
            const box = await columns.boundingBox();
            const card = board(page).getByRole('button', { name: /ACME-106/ });
            const from = await card.boundingBox();
            await page.mouse.move(from.x + from.width / 2, from.y + from.height / 2);
            await page.mouse.down();
            await page.mouse.move(box.x + box.width - 10, from.y + 40, { steps: 8 });
            await page.waitForFunction(
                () => document.querySelector('.c-board__columns').scrollLeft > 0,
            );
            await page.keyboard.press('Escape');
            await page.mouse.up();
            assert.equal(
                await page.locator('.c-board-card.is-dragged').count(),
                0,
                'Escape let the card go',
            );
        }
        assert.equal(
            (await calls(page, 'move_issue')).length,
            moves,
            'Escape moved nothing',
        );
        await boardColumn(page, 'In Review').scrollIntoViewIfNeeded();
        {
            const held = await drag(page, 'ACME-103', 'Blocked');
            await held.drop();
        }
        const confirm = page.getByRole('dialog', { name: 'Move ACME-103 out of review?' });
        await confirm.waitFor();
        assert.match(await confirm.innerText(), /This moves it to Blocked in Jira/);
        assert.equal((await calls(page, 'move_issue')).length, moves + 1);
        assert.equal((await calls(page, 'move_issue')).at(-1).args.confirmed, false);
        await boardColumn(page, 'In Review').getByText('ACME-103').waitFor();
        await shot(page, join(shots, `board-leave-review-${scheme}.png`));
        await confirm.getByRole('button', { name: 'Cancel' }).click();
        await confirm.waitFor({ state: 'detached' });
        assert.equal(
            (await calls(page, 'move_issue')).length,
            moves + 1,
            'cancelled: no move',
        );

        // ── A setup that is running shows its terminal, with what it printed before it opened ─
        await page.evaluate(() => window.settingUp('ACME-106'));
        await queue(page).getByRole('button', { name: 'List' }).click();
        await row(page, 'ACME-106').click();
        await runSection(page).getByText('Setting up its worktree').first().waitFor();
        await runSection(page).locator('.xterm-rows').getByText('step 2').waitFor();
        await shot(page, join(shots, `issue-setting-up-${scheme}.png`));

        await page.context().close();
    }

    console.log(`ok — screenshots in ${shots}`);
} finally {
    await browser?.close();
    await server.close();
}

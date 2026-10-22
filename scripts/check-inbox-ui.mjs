/**
 * Drive the real app's inbox with mocked IPC: the Work view's column, the focused Inbox and its
 * filters, a question answered on its card, a ready review's findings as links, Home's Needs you,
 * the dock badge, a toast, and — the property P1 exists for — a notification click that lands on
 * exactly what its item is about and settles nothing.
 *
 * No Tauri, no CLI, no credits. What it proves is the window's words and wiring against the shapes
 * `work_inbox` and `review_run` return; filing, dedupe and persistence are `inbox_store.rs`'s and
 * `review_engine.rs`'s.
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
const shots = process.env.WTM_SHOTS || join(tmpdir(), 'wtm-inbox');
mkdirSync(shots, { recursive: true });

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
const projects = [
  { id: '/x/webapp', name: 'webapp', root: '/x/webapp', usable: true, problem: null, trust: null },
  { id: '/x/billing', name: 'billing', root: '/x/billing', usable: true, problem: null, trust: null },
];
const id = (dir) => '/x/webapp-wt/' + dir;
const wt = (dir, branch) => ({
  id: id(dir), title: dir, subtitle: branch, path: id(dir), dirname: dir, branch, head: 'a1b2c3d4e5',
  isMain: false, isBare: false, locked: null, prunable: null, dirty: false, untracked: 0,
  staged: 0, ahead: 1, behind: 0, issueKey: null, badges: [], links: [], table: [], env: [],
  browserHome: null });
const worktrees = [
  { ...wt('webapp', 'main'), id: '/x/webapp', path: '/x/webapp', isMain: true },
  wt('export', 'fix/export'),
  wt('dates', 'fix/dates'),
];
const step = (n, role, provider, status, extra = {}) => ({ id: n, stage: 0, role, provider,
  model: null, profile: 'reviewer', workspace: 'snapshot', status, failures: 0, retries: 1,
  standsInFor: null, detail: null, resetsAt: null, skillProblem: null, result: null, ...extra });
const done = { kind: 'findings', payload: { findings: [] } };
const source = (role, inv) => ({ step: inv, invocation: inv, ordinal: 0, role, provider: 'codex',
  model: 'gpt-6', workspace: 'snapshot', treeSha: '9f00aa' });
const record = (n, flow, cls, title, path, line, extra = {}) => ({ id: n, flow, class: cls,
  severity: 'notable', location: { path, startLine: line, endLine: line }, title, rationale: 'Why.',
  requirement: null, treeSha: '9f00aa', dismissed: null, lines: true,
  evidence: [{ source: source('correctness', 2), reported: { title, severity: 'notable', class: cls.kind, body: 'x' } }], ...extra });
const run = (flow, dir, steps, findings, more = {}) => ({
  flow: { id: flow, state: { kind: 'review_mine', end: null, hold: null, phase: 'consolidated' }, startedAt: 0, updatedAt: 0 },
  review: { flow, project: '/x/webapp', worktree: id(dir), worktreePath: id(dir), recipe: 'standard', base: 'origin/main', snapshot: 1 },
  snapshot: { headSha: 'a1b2c3d4e5f6', treeSha: '9f00aa', baseSha: 'c0ffee11' },
  steps, invocations: [], inbox: [], running: false, findings, checks: [], expectedChecks: [],
  applied: { rules: [], sources: [], skills: [] },
  pullRequest: null, fixes: [], leases: [], writers: [], screenshots: [], pendingStart: null, ...more });
const runs = {
  [id('export')]: run(1, 'export', [step(11, 'conventions', 'claude', 'done', { result: done }),
      step(12, 'correctness', 'codex', 'failed', { failures: 2, detail: 'it had no result after 15 min' })], [
    record(61, 1, { kind: 'confirmed_bug', verification: 'ran unit: 1 failed' }, 'Drops the last row', 'src/totals.py', 7),
    record(62, 1, { kind: 'suggestion' }, 'Name the export job', 'src/jobs.py', 3),
  ]),
  [id('dates')]: run(2, 'dates', [step(21, 'ui', 'claude', 'waiting_capacity', { resetsAt: 1792560000 })], []),
};
const at = Date.now() - 60_000;
const item = (n, flow, kind, target, dir, title, body, more = {}) => ({ id: n, flow, kind, target,
  title, body, state: 'open', createdAt: at + n, closedAt: null, project: '/x/webapp', worktree: id(dir),
  question: null, ...more });
const question = { id: 5, flow: 1, step: 12, role: 'correctness', text: 'Is an empty export an error?',
  why: 'It decides whether the empty case is a bug or a feature.', unblocks: 'Reporting the empty case one way or the other',
  blocks: null, context: [{ kind: 'file', path: 'src/totals.py', line: 7, note: 'the new raise' }, { kind: 'mention', text: 'ExportWriter' }],
  recommendation: 'Valid, header only: it matches the other exports', options: ['An error', 'Valid, header only'],
  askedAt: at, answer: null };
window.inbox = [
  item(7, 1, 'review_ready', { kind: 'flow' }, 'export', 'Review ready, incomplete: 2 findings',
    'Confirmed bug, notable: Drops the last row (src/totals.py:7)\\nSuggestion, notable: Name the export job (src/jobs.py:3)\\n\`correctness\` failed, so this review is missing what it would have found.'),
  item(6, 2, 'capacity', { kind: 'step', step: 21, role: 'ui' }, 'dates', '\`ui\` is paused: claude limit reached', "You've hit your usage limit.\\n\\nIt resets at 3:00 PM."),
  item(5, 1, 'question', { kind: 'question', question: 5 }, 'export', '\`correctness\` asks: Is an empty export an error?', question.why, { question }),
  item(4, 1, 'approval', { kind: 'step', step: 12, role: 'correctness' }, 'export', '\`correctness\` asked for a permission its profile should not need', 'It asked to run \`git push\`.'),
  item(3, 1, 'interrupted', { kind: 'flow' }, 'export', 'wtm quit during this review', '1 of 2 roles had finished; their results are kept.'),
  item(2, 1, 'failure', { kind: 'step', step: 12, role: 'correctness' }, 'export', '\`correctness\` failed: it had no result after 15 min', 'It had no result after 15 min after 2 attempts.'),
  { ...item(1, 9, 'failure', { kind: 'flow' }, 'export', 'This review could not run', 'The snapshot could not be taken.'),
    project: '/x/billing', worktree: '/x/billing-wt/api' },
];
window.calls = [];
mockIPC((cmd, args) => {
  const a = args ?? {};
  window.calls.push({ cmd, args: a });
  switch (cmd) {
    case 'get_pref': return localStorage.getItem(prefKey(a.key));
    case 'set_pref': localStorage.setItem(prefKey(a.key), a.value); return null;
    case 'list_projects': return structuredClone(projects);
    case 'list_worktrees': return structuredClone(a.project === '/x/billing' ? [] : worktrees);
    case 'sidebar_layout': return { groups: [], origins: {} };
    case 'list_agents': return [];
    case 'list_openers': return { openers: [], preferred: null };
    case 'usage_limits': return [];
    case 'browser_available': return { runtime: false, reason: 'harness' };
    case 'notification_permission': return 'granted';
    case 'dictation_status': return { available: false, recording: false, key: null, reason: 'harness' };
    case 'doctor': return { path: [], tools: [], problems: [] };
    case 'review_recipes': return [{ id: 'standard', label: 'Standard', roles: [] }];
    case 'latest_review': return structuredClone(runs[a.worktree] ?? null);
    case 'review_run': return structuredClone(Object.values(runs).find((r) => r.flow.id === a.flow));
    case 'review_staleness': throw { kind: 'flow', message: 'no snapshot', detail: null };
    case 'work_inbox': return structuredClone(window.inbox);
    case 'resolve_inbox_item':
    case 'dismiss_inbox_item': {
      const at = window.inbox.findIndex((x) => x.id === a.item);
      if (at < 0) return null;
      const [closed] = window.inbox.splice(at, 1);
      return structuredClone({ ...closed, state: cmd === 'dismiss_inbox_item' ? 'dismissed' : 'resolved', closedAt: 1 });
    }
    case 'answer_question': {
      const at = window.inbox.findIndex((x) => x.question?.id === a.question);
      const [answered] = window.inbox.splice(at, 1);
      return structuredClone({ ...answered.question, answer: { choice: a.choice, text: a.text, at: 1 } });
    }
    case 'resume_review': return structuredClone(Object.values(runs).find((r) => r.flow.id === a.flow));
    case 'code_tree': return { files: ['src/totals.py'], ignored: [], dirs: [], symlinks: [], missing: [], truncated: false };
    case 'code_read_file': return { text: 'def total():\\n    pass\\n', size: 20, truncated: false, mtimeMs: 1, symlink: false, outside: false };
    case 'code_changes': return { scope: 'uncommitted', against: 'HEAD', rev: 'HEAD', changes: [] };
    default: return cmd.startsWith('list_') ? [] : null;
  }
}, { shouldMockEvents: true });
const { mount } = await import('svelte');
await import('/src/styles/main.scss');
const { default: App } = await import('/src/App.svelte');
const { workspace } = await import('/src/lib/state/workspace.svelte.ts');
const { work } = await import('/src/lib/state/work.svelte.ts');
const { view } = await import('/src/lib/state/view.svelte.ts');
const { inbox } = await import('/src/lib/state/inbox.svelte.ts');
window.harness = { emit, workspace, work, view, inbox };
mount(App, { target: document.getElementById('app') });`;

const server = await createServer({
    configFile: false,
    envDir: false,
    logLevel: 'warn',
    plugins: [
        svelte(),
        {
            name: 'inbox-probe',
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
            viewport: { width: 1440, height: 900 },
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
        await page.waitForFunction(
            () =>
                window.harness?.workspace.worktrees.length > 0 &&
                window.harness.inbox.items.length === 7,
        );
        await page.getByRole('region', { name: 'Work', exact: true }).waitFor();
        return page;
    }

    const shot = (page, path) => page.screenshot({ path, animations: 'disabled' });
    const calls = (page, cmd) =>
        page.evaluate((c) => window.calls.filter((x) => x.cmd === c), cmd);
    const closing = async (page) =>
        (await calls(page, 'resolve_inbox_item')).length +
        (await calls(page, 'dismiss_inbox_item')).length;
    const column = (page) => page.getByRole('complementary', { name: /Inbox/ });
    const chip = (page, name) =>
        page.getByRole('group', { name: 'Filter' }).getByRole('button', { name });
    /** Wait for `selector`, and say where the window was if it never came. */
    async function arrive(page, selector) {
        try {
            await page.locator(selector).waitFor({ timeout: 5000 });
        } catch (error) {
            const where = await page.evaluate((s) => {
                const h = window.harness;
                return {
                    view: h.view.current,
                    picked: h.work.picked,
                    tab: h.work.tab,
                    focus: h.work.focus,
                    inboxOpen: h.work.inboxOpen,
                    found: document.querySelector(s.split('.')[0])?.outerHTML?.slice(0, 200) ?? null,
                };
            }, selector);
            throw new Error(`${selector} never arrived: ${JSON.stringify(where)}`, { cause: error });
        }
    }

    for (const scheme of ['light', 'dark']) {
        const page = await open(scheme);
        await page.evaluate(() => window.harness.work.pick('/x/webapp-wt/dates'));

        // ── The column: every open item, every project's, newest first ───────────────────────
        const text = await column(page).textContent();
        assert(text.includes('Inbox 7 open'), text);
        assert(text.includes('billing / api'), 'another project’s item names its project');
        assert.equal(
            await column(page).locator('.c-run-inbox__kind').first().textContent(),
            'Review ready',
        );
        // The dock badge counts the inbox beside waiting sessions.
        const badge = (await calls(page, 'plugin:window|set_badge_count')).at(-1);
        assert.equal(badge?.args.value, 7, JSON.stringify(badge));
        // The title bar's Work segment carries the same count.
        assert(
            (await page.locator('.c-view-switch').textContent()).includes('7'),
            'the Work segment shows the inbox count',
        );
        await shot(page, join(shots, `column-${scheme}.png`));

        // ── The focused Inbox, from the title bar, with its filters ─────────────────────────
        await page.getByRole('button', { name: /^Inbox/ }).click();
        const list = page.getByRole('region', { name: 'Inbox' });
        await list.waitFor();
        const chips = await page.locator('.c-inbox-filters__chip').allTextContents();
        assert.deepEqual(
            chips.map((c) => c.replace(/\s+/g, ' ').trim()),
            ['All 7', 'Questions 1', 'Approvals 1', 'Failures 3', 'Usage 1'],
        );
        await chip(page, /^Failures/).click();
        assert.equal(await page.locator('.c-inbox-list__row').count(), 3);
        await chip(page, /^Usage/).click();
        assert.equal(await page.locator('.c-inbox-list__row').count(), 1);
        const usage = await page.locator('.c-inbox-card').textContent();
        assert(usage.includes('Try now'), usage);
        assert(usage.includes('Show ui'), usage);
        await shot(page, join(shots, `focused-usage-${scheme}.png`));

        // ── An approval whose session has gone says so, and offers nothing to press ─────────
        await chip(page, /^Approvals/).click();
        const approval = await page.locator('.c-inbox-card').textContent();
        assert(approval.includes('nothing left to answer'), approval);

        // ── A ready review: its findings are links to the exact finding ─────────────────────
        await chip(page, /^All/).click();
        await page.locator('.c-inbox-list__row').first().click();
        await page.locator('.c-inbox-card__findings').first().waitFor();
        const ready = await page.locator('.c-inbox-card').textContent();
        assert(ready.includes('Confirmed bugs · 1'), ready);
        await shot(page, join(shots, `focused-ready-${scheme}.png`));
        await page.getByRole('button', { name: 'Drops the last row' }).click();
        await arrive(page, '#work-finding-61.is-targeted');
        assert.equal(await page.evaluate(() => window.harness.work.inboxOpen), false);
        // Read on screen in a focused window: the ready item is settled, and only it.
        await page.waitForFunction(() =>
            window.calls.some((c) => c.cmd === 'resolve_inbox_item' && c.args.item === 7),
        );
        assert.equal(await closing(page), 1);
        await shot(page, join(shots, `click-to-finding-${scheme}.png`));

        // ── A notification click lands on the role it is about, and settles nothing ──────────
        const settled = await closing(page);
        await page.evaluate(() =>
            window.harness.emit('notification:clicked', {
                projectId: '/x/webapp',
                worktreeId: '/x/webapp-wt/export',
                paneId: 'work:run',
                inboxItem: 2,
            }),
        );
        await arrive(page, '#work-role-12.is-targeted');
        assert.equal(await closing(page), settled, 'arriving answers nothing');
        assert(await page.evaluate(() => window.harness.inbox.find(2) !== null));
        assert(await page.locator('#work-role-12').evaluate((e) => e === document.activeElement));
        await shot(page, join(shots, `click-to-role-${scheme}.png`));

        // ── The question, from a notification: the card, then an answer ─────────────────────
        await page.evaluate(() =>
            window.harness.emit('notification:clicked', {
                projectId: '/x/webapp',
                worktreeId: '/x/webapp-wt/export',
                paneId: 'work:run',
                inboxItem: 5,
            }),
        );
        const card = page.locator('.c-question');
        await card.waitFor();
        const q = await card.textContent();
        for (const words of [
            'Is an empty export an error?',
            'Why it matters',
            'Unblocks',
            'Reporting the empty case one way or the other',
            'src/totals.py:7',
            'ExportWriter',
            'Recommendation:',
            'recommended',
            'went ahead on its own reading',
        ])
            assert(q.includes(words), `${words} in ${q}`);
        assert(await card.getByRole('button', { name: 'Discuss' }).isDisabled());
        assert(
            await card.getByRole('radio', { name: /Valid, header only/ }).isChecked(),
            'the recommended option is picked to start with',
        );
        await shot(page, join(shots, `question-${scheme}.png`));
        await card.getByRole('textbox').fill('and log it');
        await card.getByRole('button', { name: 'Answer' }).click();
        await page.waitForFunction(() => !window.harness.inbox.find(5));
        assert.deepEqual((await calls(page, 'answer_question')).at(-1).args, {
            question: 5,
            choice: 1,
            text: 'and log it',
        });

        // ── Home's Needs you lists the runs' items beside the panes' ────────────────────────
        await page.evaluate(() => window.harness.view.show('home'));
        const needs = page.getByRole('region', { name: 'Needs you' });
        await needs.waitFor();
        const home = await needs.textContent();
        assert(home.includes('Review run in dates'), home);
        assert(home.includes('billing / api'), home);
        await shot(page, join(shots, `home-needs-you-${scheme}.png`));
        // No toast while Home is up: its list is the summary a toast stands in for.
        await page.evaluate(() =>
            window.harness.emit('flow:inbox', {
                id: 8, flow: 2, kind: 'failure', target: { kind: 'step', step: 21, role: 'ui' },
                title: '`ui` failed: its session ended', body: 'x', state: 'open', createdAt: Date.now(),
                closedAt: null, project: '/x/webapp', worktree: '/x/webapp-wt/dates', question: null,
            }),
        );
        await page.waitForFunction(() => window.harness.inbox.find(8) !== null);
        assert.equal(await page.locator('.c-toast').count(), 0);

        // ── In the Worktrees view, a newly filed item is a toast that goes to its role ──────
        await page.evaluate(() => window.harness.view.show('worktree'));
        await page.evaluate(() =>
            window.harness.emit('flow:inbox', {
                id: 9, flow: 2, kind: 'capacity', target: { kind: 'step', step: 21, role: 'ui' },
                title: '`ui` is paused again', body: 'x', state: 'open', createdAt: Date.now(),
                closedAt: null, project: '/x/webapp', worktree: '/x/webapp-wt/dates', question: null,
            }),
        );
        const toast = page.locator('.c-toast').first();
        await toast.waitFor();
        assert((await toast.textContent()).includes('dates: review paused for usage'));
        const before = await closing(page);
        await toast.locator('.c-toast__body').click();
        await arrive(page, '#work-role-21.is-targeted');
        assert.equal(await closing(page), before, 'following a toast settles nothing');
        await page.context().close();
    }

    console.log(`ok — screenshots in ${shots}`);
} finally {
    await browser?.close();
    await server.close();
}

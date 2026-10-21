/**
 * Drive the real app's Work view with mocked IPC through every state a review run's readout draws:
 * not reviewed (with a recipe to pick), running, waiting for usage capacity, interrupted by a quit,
 * a role that failed, and reviewed. Plus the run inbox, Run Review's request, and Resume's.
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
];
const findings = (n) => ({ kind: 'findings', payload: { findings: Array.from({ length: n }, (_, i) => ({
  title: 'Finding ' + i, severity: 'notable', class: 'suggestion', body: 'x' })) } });
const step = (n, role, provider, status, extra = {}) => ({ id: n, stage: 0, role, provider,
  model: provider === 'claude' ? 'sonnet' : null, status, failures: 0, retries: 1,
  standsInFor: null, detail: null, resetsAt: null, result: null, ...extra });
const invocation = (n, stepId, role, status, extra = {}) => ({ id: n, step: stepId, attempt: 1, role,
  status, detail: null, progress: null, startedAt: 0, endedAt: null, ...extra });
const run = (flow, worktree, state, steps, invocations = [], inbox = [], running = false) => ({
  flow: { id: flow, state: { kind: 'review_mine', end: null, hold: null, ...state }, startedAt: 0, updatedAt: 0 },
  review: { flow, project: '/x/webapp', worktree: id(worktree), worktreePath: id(worktree), recipe: 'standard', base: 'origin/main' },
  snapshot: { headSha: 'a1b2c3d4e5f6', treeSha: '9f00aa', baseSha: 'c0ffee11' },
  steps, invocations, inbox, running });
const runs = {
  [id('running')]: run(1, 'running', { phase: 'reviewing' }, [
      step(11, 'conventions', 'claude', 'done', { result: findings(2) }),
      step(12, 'correctness', 'codex', 'running'),
      step(13, 'ui', 'claude', 'skipped', { detail: 'none of its paths changed' }),
    ], [invocation(1, 12, 'correctness', 'running', { progress: 'reading export.py', attempt: 2 })], [], true),
  [id('waiting')]: run(2, 'waiting', { phase: 'reviewing', hold: { kind: 'waiting_capacity', provider: 'codex', resetsAt: 1792560000 } }, [
      step(21, 'conventions', 'claude', 'done', { result: findings(1) }),
      step(22, 'correctness', 'codex', 'waiting_capacity', { resetsAt: 1792560000, detail: "You've hit your usage limit." }),
    ], [], [], true),
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
      step(53, 'correctness-claude', 'claude', 'done', { result: findings(1), standsInFor: 'correctness' }),
    ]),
};
const inbox = [
  { id: 1, flow: 2, kind: 'capacity', subject: 'correctness', title: '\`correctness\` is paused: Codex limit reached', body: "You've hit your usage limit.", createdAt: 0 },
  { id: 2, flow: 4, kind: 'failure', subject: 'ui', title: '\`ui\` failed: it had no result after 15 min', body: 'It had no result after 15 min after 2 attempts.', createdAt: 0 },
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
    case 'start_review': return structuredClone({ ...runs[id('running')], review: { ...runs[id('running')].review, worktree: a.request.worktree, recipe: a.request.recipe } });
    case 'resume_review': return structuredClone({ ...Object.values(runs).find((r) => r.flow.id === a.flow), running: true, flow: { ...Object.values(runs).find((r) => r.flow.id === a.flow).flow, state: { kind: 'review_mine', end: null, hold: null, phase: 'reviewing' } } });
    case 'work_inbox': return structuredClone(inbox);
    case 'resolve_inbox_item': return true;
    default: return cmd.startsWith('list_') ? [] : null;
  }
});
const { mount } = await import('svelte');
await import('/src/styles/main.scss');
const { default: App } = await import('/src/App.svelte');
const { workspace } = await import('/src/lib/state/workspace.svelte.ts');
const { work } = await import('/src/lib/state/work.svelte.ts');
window.harness = { emit, workspace, work };
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
                    ?.textContent?.includes(id === '/x/webapp' ? 'main' : id.split('/').at(-1)),
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
        const inboxText = await page.getByRole('complementary', { name: /Inbox/ }).textContent();
        assert(inboxText.includes('Inbox 2'), inboxText);
        assert(inboxText.includes('correctness is paused: Codex limit reached'), inboxText);
        assert.equal(
            await page.locator('.c-run-inbox__title code').first().textContent(),
            'correctness',
            'a backticked name is drawn as code, not with its backticks',
        );
        assert(inboxText.includes('Failure'));

        // ── Not reviewed, with two recipes to pick from ───────────────────────────────────────
        await pick(page, 'webapp');
        await readout(page, 'Not reviewed yet', 'Review main');
        const select = panel(page).getByRole('combobox', { name: 'Recipe' });
        assert.equal(await select.inputValue(), 'standard');
        assert((await panel(page).textContent()).includes('conventions on Claude sonnet'));
        await shot(page, join(shots, `not-reviewed-${scheme}.png`));

        // ── Running: one role done, one on its second attempt, one skipped ───────────────────
        await pick(page, 'running');
        let text = await readout(page, 'Running', 'Reviewing snapshot a1b2c3d + uncommitted');
        assert(text.includes('done · 2 findings'), text);
        assert(text.includes('reading export.py · attempt 2'), text);
        assert(text.includes('Skipped, because none of their paths changed: ui.'), text);
        assert(await panel(page).getByRole('button', { name: 'Stop review' }).isVisible());
        assert.equal(
            await page.locator('.c-work__header .c-badge').textContent(),
            'Running',
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
        await shot(page, join(shots, `waiting-capacity-${scheme}.png`));

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
        assert(await panel(page).getByRole('button', { name: 'Retry failed roles' }).isVisible());
        await shot(page, join(shots, `role-failed-${scheme}.png`));

        // ── Reviewed, with a fallback that stood in for a role ──────────────────────────────
        await pick(page, 'reviewed');
        text = await readout(page, 'Reviewed', '1 finding from 2 roles');
        assert(text.includes('correctness-claude (for correctness)'), text);
        assert(text.includes('handed over'), text);
        await shot(page, join(shots, `reviewed-${scheme}.png`));

        // ── Run Review sends the recipe picked ───────────────────────────────────────────────
        await pick(page, 'webapp');
        await panel(page).getByRole('combobox', { name: 'Recipe' }).selectOption('quick');
        await panel(page).getByRole('button', { name: 'Run Review' }).click();
        assert.deepEqual((await calls(page, 'start_review')).at(-1).args, {
            request: { project: '/x/webapp', worktree: '/x/webapp', recipe: 'quick' },
        });

        // ── Resolving an inbox item takes it away ────────────────────────────────────────────
        await page
            .getByRole('complementary', { name: /Inbox/ })
            .getByRole('button', { name: 'Resolve' })
            .first()
            .click();
        await page.waitForFunction(() =>
            document.querySelector('.c-work__inbox')?.textContent?.includes('Inbox 1'),
        );
        assert.deepEqual((await calls(page, 'resolve_inbox_item')).at(-1).args, { item: 1 });
        await page.context().close();
    }

    console.log(`ok — screenshots in ${shots}`);
} finally {
    await browser?.close();
    await server.close();
}

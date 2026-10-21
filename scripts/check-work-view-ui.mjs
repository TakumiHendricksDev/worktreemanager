/**
 * Drive the real app shell with mocked IPC: the Work view behind `ui.work_view`, and the two views
 * that existed before it. No Tauri, no CLI, no credits.
 *
 * What it proves, in order: with the preference off nothing about the title bar or the views
 * changed, and a remembered Work view cannot open; turning it on in Settings adds the switch and
 * saves through `set_work_view`; Work draws the queue, the workspace and the inbox from the listing
 * alone; the preference and the view survive a reload; every route out of Work lands where it says;
 * and turning it off again takes Work away, even from the Go menu's event.
 *
 * `WTM_PLAYWRIGHT_MODULE` names a Playwright module, as for `check-design-first-ui.mjs`.
 * Screenshots go to `WTM_SHOTS`, or a directory under the system temp dir, in light and dark.
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
const shots = process.env.WTM_SHOTS || join(tmpdir(), 'wtm-work-view');
mkdirSync(shots, { recursive: true });

/*
 * The page: the real `App`, after a mock of every command it calls on the way up. Preferences live
 * in `localStorage` under `harness.pref.*`, so a reload stands in for a relaunch reading the config
 * file. Unknown commands answer null, or an empty list for a `list_*`, which is what an app with
 * nothing running would say.
 */
const entry = `
import { emit } from '@tauri-apps/api/event';
import { mockIPC, mockWindows } from '@tauri-apps/api/mocks';
const params = new URLSearchParams(location.search);
const prefKey = (k) => 'harness.pref.' + k;
if (localStorage.getItem(prefKey('ui.notify')) === null) localStorage.setItem(prefKey('ui.notify'), 'off');
if (params.get('theme')) localStorage.setItem(prefKey('ui.theme'), params.get('theme'));
document.documentElement.dataset.theme = localStorage.getItem(prefKey('ui.theme')) ?? 'light';
document.documentElement.dataset.palette = 'pine';
document.documentElement.dataset.platform = 'macos';
mockWindows('main');
const project = { id: '/x/webapp', name: 'webapp', root: '/x/webapp', usable: true, problem: null, trust: null };
const wt = (dir, branch, extra = {}) => ({
  id: dir === 'webapp' ? '/x/webapp' : '/x/webapp-wt/' + dir, title: dir, subtitle: branch ?? '(detached)',
  path: dir === 'webapp' ? '/x/webapp' : '/x/webapp-wt/' + dir, dirname: dir, branch, head: 'a1b2c3d4e5',
  isMain: dir === 'webapp', isBare: false, locked: null, prunable: null, dirty: false, untracked: 0,
  staged: 0, ahead: 0, behind: 0, issueKey: null, badges: [], links: [], table: [], env: [],
  browserHome: null, ...extra });
const worktrees = [
  wt('webapp', 'main'),
  wt('APP-8701-bulk-export', 'feature/APP-8701-bulk-export', { issueKey: 'APP-8701', dirty: true, staged: 1, untracked: 2, ahead: 3 }),
  wt('fix-date-picker', 'fix/date-picker'),
  wt('gone', 'old/gone', { prunable: 'gitdir file points to non-existent location' }),
];
// One running session, blocked on an approval, so the title bar has a count to carry.
const live = [{ session: 's1', project: '/x/webapp', worktree: '/x/webapp-wt/fix-date-picker', provider: 'claude', providerSession: 'p1' }];
const replay = [
  { kind: 'user_echo', text: 'Fix the date picker' },
  { kind: 'turn_started', turn: 't' },
  { kind: 'approval_requested', id: 'a1', blocking: true, request: { kind: 'command', command: 'npm test', cwd: '/x/webapp-wt/fix-date-picker', reason: null } },
];
window.calls = [];
mockIPC((cmd, args) => {
  const a = args ?? {};
  window.calls.push({ cmd, args: a });
  switch (cmd) {
    case 'get_pref': return localStorage.getItem(prefKey(a.key));
    case 'set_pref': localStorage.setItem(prefKey(a.key), a.value); return null;
    case 'set_work_view': localStorage.setItem(prefKey('ui.work_view'), a.enabled ? 'on' : 'off'); return null;
    case 'list_projects': return [structuredClone(project)];
    case 'list_worktrees': return structuredClone(worktrees);
    case 'sidebar_layout': return { groups: [], origins: {} };
    case 'list_agents': return [{ id: 'claude', label: 'Claude Code', blurb: '', available: true, offered: true, detail: null, model: null, effort: null, mode: null }];
    case 'list_agent_sessions': return structuredClone(live);
    case 'agent_replay': return a.session === 's1' ? replay.map((event, seq) => ({ seq, event: structuredClone(event) })) : [];
    case 'home_jobs': return [];
    case 'list_openers': return { openers: [], preferred: null };
    case 'usage_limits': return [];
    case 'browser_available': return { runtime: false, reason: 'harness' };
    case 'notification_permission': return 'granted';
    case 'dictation_status': return { available: false, recording: false, key: null, reason: 'harness' };
    case 'doctor': return { path: [], tools: [], problems: [] };
    // A project with no [workflow] forge: the Reviews section says why it offers nothing to add.
    case 'review_queue': return { forge: null, items: [] };
    default: return cmd.startsWith('list_') ? [] : null;
  }
}, { shouldMockEvents: true });
await import('@xterm/xterm/css/xterm.css');
await import('/src/styles/main.scss');
const { mount } = await import('svelte');
const { default: App } = await import('/src/App.svelte');
const { attention } = await import('/src/lib/state/attention.svelte.ts');
const { view } = await import('/src/lib/state/view.svelte.ts');
const { workspace } = await import('/src/lib/state/workspace.svelte.ts');
window.harness = { emit, attention, view, workspace };
mount(App, { target: document.getElementById('app') });`;

const server = await createServer({
    configFile: false,
    envDir: false,
    logLevel: 'warn',
    plugins: [
        svelte(),
        {
            name: 'work-view-probe',
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
            viewport: { width: 1280, height: 800 },
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
        await booted(page);
        return page;
    }

    /** Booted is the worktree list in the sidebar, or a view that hides it. */
    async function booted(page) {
        await page.waitForFunction(() => window.harness?.workspace.worktrees.length > 0);
        await page.waitForFunction(() => !document.body.textContent.includes('Starting…'));
    }

    /** Transitions finished first: a row's background is otherwise caught halfway to selected. */
    const shot = (page, path) => page.screenshot({ path, animations: 'disabled' });
    const calls = (page, cmd) =>
        page.evaluate((c) => window.calls.filter((x) => x.cmd === c), cmd);
    const views = (page) => page.getByRole('navigation', { name: 'Views' });
    const homeButton = (page) => page.locator('.c-titlebar__home button');
    const sidebar = (page) => page.getByRole('navigation', { name: 'Worktrees' });
    const queue = (page) => page.getByRole('navigation', { name: 'Work queue' });
    const workSection = (page) => page.getByRole('region', { name: 'Work', exact: true });
    const homeSection = (page) => page.locator('section.c-home');
    const offScreenNow = (page) =>
        page.evaluate(() =>
            window.harness.attention.offScreen(window.harness.workspace.selectedWorktreeId),
        );

    async function setWorkView(page, on) {
        await page.getByRole('button', { name: 'Settings', exact: true }).click();
        await page.getByRole('tab', { name: 'General' }).click();
        const box = page.getByRole('checkbox', { name: /Show the Work view/ });
        assert.equal(
            await box.isChecked(),
            !on,
            'the checkbox should start in the other state',
        );
        await box.click();
        await page.keyboard.press('Escape');
        await page.locator('[aria-modal="true"]').waitFor({ state: 'detached' });
    }

    // ── Off, the default: the title bar and both existing views are as they were ──────────────
    for (const scheme of ['light', 'dark']) {
        const page = await open(`?theme=${scheme}`, scheme);
        assert.equal(await views(page).count(), 0, 'no view switch while Work is off');
        assert.equal(await homeButton(page).getAttribute('aria-pressed'), 'false');
        assert(await sidebar(page).isVisible(), 'the worktree view opens by default');
        assert.equal(await queue(page).count(), 0, 'Work is not mounted while off');
        await shot(page, join(shots, `off-worktree-${scheme}.png`));
        await homeButton(page).click();
        await homeSection(page).waitFor({ state: 'visible' });
        await shot(page, join(shots, `off-home-${scheme}.png`));
        await homeButton(page).click();
        await sidebar(page).waitFor({ state: 'visible' });
        // The Go menu's event cannot open a view that does not exist.
        await page.evaluate(() => window.harness.emit('wtm:work'));
        assert.equal(await page.evaluate(() => window.harness.view.current), 'worktree');
        await page.context().close();
    }

    // A remembered Work view whose preference has since been turned off lands on the worktree view.
    {
        const page = await open();
        await page.evaluate(() => localStorage.setItem('wtm.view', 'work'));
        await page.reload();
        await booted(page);
        assert(await sidebar(page).isVisible());
        assert.equal(await queue(page).count(), 0);
        assert.equal(
            await page.evaluate(() => localStorage.getItem('wtm.view')),
            'worktree',
        );

        // ── On, from Settings ───────────────────────────────────────────────────────────────
        await setWorkView(page, true);
        assert.deepEqual(
            (await calls(page, 'set_work_view')).map((c) => c.args),
            [{ enabled: true }],
        );
        await views(page).waitFor();
        assert.equal(
            await page
                .getByRole('button', { name: 'Worktrees', exact: true })
                .getAttribute('aria-current'),
            'page',
        );
        assert.equal(
            await homeButton(page).count(),
            0,
            'the switch replaces the Home button',
        );
        assert.equal(
            await views(page).getByRole('button', { name: /^Home/ }).getAttribute('aria-label'),
            'Home — 1 waiting on you (⇧⌘H)',
            'the count the Home button carried rides on its segment',
        );
        await shot(page, join(shots, 'on-worktree-light.png'));
        assert.equal(await offScreenNow(page), false, 'the worktree view shows its panes');

        // ── Work: queue, workspace, inbox, from the listing alone ──────────────────────────
        await page.getByRole('button', { name: 'Work', exact: true }).click();
        await workSection(page).waitFor({ state: 'visible' });
        assert(await queue(page).isVisible());
        assert(!(await sidebar(page).isVisible()), 'the queue takes the sidebar’s place');
        assert.equal(await page.evaluate(() => localStorage.getItem('wtm.view')), 'work');
        assert.equal(await offScreenNow(page), true, 'Work shows no pane');
        const rows = queue(page).locator('.c-work-row');
        assert.equal(
            await rows.count(),
            3,
            'bare and prunable worktrees are not reviewable',
        );
        assert.deepEqual(await rows.locator('.c-work-row__status').allTextContents(), [
            'Not reviewed',
            'Not reviewed',
            'Not reviewed',
        ]);
        assert(
            (await queue(page).textContent()).includes('names no forge'),
            'Reviews is empty and says why',
        );
        const title = page.locator('.c-work__title');
        // Arriving from the worktree view lands on the worktree selected there.
        const selected = await page.evaluate(() => window.harness.workspace.selected.id);
        assert.equal(
            await rows.and(page.locator('[aria-current="true"]')).getAttribute('title'),
            await page.evaluate(
                (id) =>
                    window.harness.workspace.worktrees
                        .filter((w) => w.id === id)
                        .map((w) => [w.title, w.branch, w.path].join('\n'))[0],
                selected,
            ),
        );
        assert.equal(
            await page.getByRole('tab', { name: 'Reviews' }).getAttribute('aria-selected'),
            'true',
        );
        await page.getByText('Not reviewed yet', { exact: true }).waitFor();
        assert(await page.getByRole('button', { name: 'Run Review' }).isDisabled());
        assert(
            (
                await page.getByRole('complementary', { name: /Inbox/ }).textContent()
            ).includes('Nothing needs you.'),
        );
        await rows.nth(1).click();
        assert.equal(await title.textContent(), 'APP-8701 · bulk export');
        assert.deepEqual(
            await rows.evaluateAll((els) => els.map((e) => e.getAttribute('aria-current'))),
            [null, 'true', null],
            'the queue marks the row the workspace shows',
        );
        assert(
            (await page.locator('.c-work__meta').textContent()).includes(
                'Uncommitted changes, 1 staged · 2 untracked',
            ),
        );
        await shot(page, join(shots, 'work-reviews-light.png'));

        // Tabs: click, then the arrow keys move both selection and focus.
        await page.getByRole('tab', { name: 'Overview' }).click();
        const facts = await page.locator('.c-work__panel dl').textContent();
        assert(
            facts.includes('feature/APP-8701-bulk-export') &&
                facts.includes('3 ahead, 0 behind') &&
                facts.includes('APP-8701, from the branch name'),
        );
        await shot(page, join(shots, 'work-overview-light.png'));
        await page.keyboard.press('ArrowRight');
        assert.equal(
            await page.evaluate(() => document.activeElement?.textContent?.trim()),
            'Reviews',
        );

        // ⌘I describes the sidebar's selection, which is not what Work is showing.
        await page.keyboard.press('Meta+i');
        assert.equal(await page.locator('[aria-modal="true"]').count(), 0);

        // ── Survives a reload, which stands in for a relaunch ─────────────────────────────
        await page.reload();
        await booted(page);
        await workSection(page).waitFor({ state: 'visible' });
        await views(page).waitFor();

        // ── Every way out lands where it says ─────────────────────────────────────────────
        await page.getByRole('button', { name: /^Home/ }).click();
        await homeSection(page).waitFor({ state: 'visible' });
        assert(!(await workSection(page).isVisible()));
        await page.getByRole('button', { name: 'Worktrees', exact: true }).click();
        await sidebar(page).waitFor({ state: 'visible' });
        await page.evaluate(() => window.harness.emit('wtm:work'));
        await workSection(page).waitFor({ state: 'visible' });
        await page.evaluate(() => window.harness.emit('wtm:work'));
        await sidebar(page).waitFor({ state: 'visible' });
        await page.getByRole('button', { name: 'Work', exact: true }).click();

        // The create form opened from Work closes back to Work.
        await queue(page).getByRole('button', { name: 'New Worktree' }).click();
        await page.getByRole('heading', { name: 'New Worktree' }).waitFor();
        await page.getByRole('button', { name: 'Cancel', exact: true }).click();
        await workSection(page).waitFor({ state: 'visible' });

        // Open in Worktrees selects the row's worktree in the sidebar and shows its sessions.
        await rows.nth(2).click();
        await page.getByRole('button', { name: 'Open in Worktrees' }).click();
        await sidebar(page).waitFor({ state: 'visible' });
        assert.equal(
            await page.evaluate(() => window.harness.workspace.selectedWorktreeId),
            '/x/webapp-wt/fix-date-picker',
        );

        // ── Off again: Work leaves with it, and so does the Go menu's route in ───────────────
        await page.getByRole('button', { name: 'Work', exact: true }).click();
        await setWorkView(page, false);
        await sidebar(page).waitFor({ state: 'visible' });
        assert.equal(await views(page).count(), 0);
        assert.equal(await homeButton(page).count(), 1);
        assert.deepEqual(
            (await calls(page, 'set_work_view')).map((c) => c.args),
            [{ enabled: false }],
            'the reload reset the log, so only the second save is in it',
        );
        await page.evaluate(() => window.harness.emit('wtm:work'));
        assert.equal(await page.evaluate(() => window.harness.view.current), 'worktree');
        await page.context().close();
    }

    // ── Dark, with the preference stored on, at the narrowest window ─────────────────────────
    for (const [width, height, name] of [
        [1280, 800, 'work-reviews-dark.png'],
        [860, 640, 'work-narrow-dark.png'],
    ]) {
        const page = await open('?theme=dark', 'dark');
        await page.evaluate(() => {
            localStorage.setItem('harness.pref.ui.work_view', 'on');
            localStorage.setItem('wtm.view', 'work');
        });
        // A fresh window at this size rather than a resize: a capture straight after
        // `setViewportSize` composites the old frame into the new one.
        await page.setViewportSize({ width, height });
        await page.reload();
        await booted(page);
        await workSection(page).waitFor({ state: 'visible' });
        const inbox = await page.locator('.c-work__inbox').boundingBox();
        const main = await page.locator('.c-work__main').boundingBox();
        if (width < 1000)
            assert(
                inbox.y >= main.y + main.height - 1,
                'narrow, the inbox sits below the workspace',
            );
        else assert(inbox.x >= main.x + main.width - 1, 'wide, it sits beside it');
        await shot(page, join(shots, name));
        await page.context().close();
    }

    console.log(`ok — screenshots in ${shots}`);
} finally {
    await browser?.close();
    await server.close();
}

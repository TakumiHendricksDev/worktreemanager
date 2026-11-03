/** Exercise a New Worktree run that outlives its screen, in the real App with mocked IPC: the ghost
 * tab, leaving and coming back, progress routed by creation id, the record a made worktree keeps,
 * a refused attempt's form, and a setup that failed while nobody was looking. `create_worktree` is
 * held open until the probe settles it; nothing is created and no command runs.
 *
 *   WTM_PLAYWRIGHT_MODULE=/path/to/playwright/index.mjs [WTM_SHOTS=dir] node scripts/check-creation-ui.mjs
 */
import assert from 'node:assert/strict';
import { pathToFileURL } from 'node:url';
import { createServer } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

if (!process.env.WTM_PLAYWRIGHT_MODULE)
    throw new Error('Set WTM_PLAYWRIGHT_MODULE to an existing Playwright module.');
const { chromium } = await import(pathToFileURL(process.env.WTM_PLAYWRIGHT_MODULE).href);
const shots = process.env.WTM_SHOTS;

const entry = `
import { emit } from '@tauri-apps/api/event';
import { mockIPC, mockWindows } from '@tauri-apps/api/mocks';
const theme = new URLSearchParams(location.search).get('theme') ?? 'dark';
document.documentElement.dataset.theme = theme;
localStorage.setItem('wtm.view', 'worktree');
mockWindows('main');
const wt = (dir, branch, main = false, key = null) => {
  const id = main ? '/x/acme' : '/x/acme-wt/' + dir;
  return { id, title: dir, subtitle: branch, path: id, dirname: dir, branch, head: 'abc1234', isMain: main, isBare: false,
    locked: null, prunable: null, dirty: false, untracked: 0, staged: 0, ahead: 0, behind: 0, issueKey: key,
    badges: [], links: [], table: [], env: [], browserHome: null };
};
const state = window.checkState = {
  calls: [], creates: [],
  worktrees: [wt('acme', 'develop', true), wt('ACME-1198-fix-dates', 'fix/ACME-1198-fix-dates', false, 'ACME-1198')],
  wt,
};
const field = (key, label) => ({ key, label, kind: 'text', required: true, default: null, placeholder: null, help: null,
  allowCustom: false, hasDynamicOptions: false, options: [], pattern: null, patternMessage: null });
const preview = (issue) => {
  const dir = issue + '-export';
  return { branch: 'task/' + dir, directory: '/x/acme-wt/' + dir, baseRef: 'origin/develop', baseCommit: 'abc1234',
    willFetch: true, gitArgv: ['git', 'worktree', 'add', '../' + dir], setupArgv: ['just', 'setup'], setupCwd: '/x/acme',
    preflight: [], warnings: [], lookups: {}, computed: {}, branchChoices: [], namingFields: ['issue'],
    normalized: { issue }, canCreate: true, issueKey: issue };
};
mockIPC((cmd, a) => {
  state.calls.push({ cmd, args: a ? structuredClone(a) : a });
  switch (cmd) {
    case 'get_pref': return localStorage.getItem('probe.pref.' + a.key);
    case 'set_pref': localStorage.setItem('probe.pref.' + a.key, a.value); return null;
    case 'list_projects': return [{ id: '/x/acme', name: 'acme', root: '/x/acme', usable: true, problem: null, trust: null }];
    case 'list_worktrees': return structuredClone(state.worktrees);
    case 'sidebar_layout': return { groups: [], origins: {} };
    case 'list_agents': return [['claude', 'Claude Code'], ['codex', 'Codex']].map(([id, label]) => ({ id, label, blurb: '', available: true, offered: true, detail: null, model: null, effort: null, mode: null }));
    case 'agent_capability': return { models: [], modes: [], modelsAreLive: false, supportsFast: false, steersMidTurn: true };
    case 'list_openers': return { openers: [], preferred: null };
    case 'browser_available': return { runtime: false, reason: 'probe' };
    case 'notification_permission': return 'granted';
    case 'dictation_status': return { available: false, recording: false, key: null, reason: 'probe' };
    case 'list_recipes': return { recipes: [], problems: [], offers: {} };
    case 'worktree_form': return { projectId: '/x/acme', actions: [], fields: [field('issue', 'Jira issue')] };
    case 'preview_worktree': return preview(a.values.issue || 'ACME-0');
    case 'create_worktree': return new Promise((resolve, reject) => state.creates.push({ args: a, resolve, reject }));
    case 'terminal_replay': return null;
    case 'usage_limits': return [];
    default: return cmd.startsWith('list_') || cmd.endsWith('_runs') || cmd === 'agent_messages' || cmd === 'home_jobs' ? [] : null;
  }
}, { shouldMockEvents: true });
window.progress = (payload) => emit('wtm:progress', payload);
window.focusWindow = () => window.dispatchEvent(new Event('focus'));
const { mount } = await import('svelte');
const { default: App } = await import('/src/App.svelte');
await import('/src/styles/main.scss');
mount(App, { target: document.getElementById('app') });
document.body.dataset.ready = 'yes';`;

const server = await createServer({
    configFile: false,
    envDir: false,
    plugins: [
        svelte(),
        {
            name: 'creation-probe',
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
                        '<!doctype html><html data-palette="pine"><body style="margin:0;height:100vh"><div id="app" style="height:100vh"></div><script type="module" src="/probe-entry.js"></script></body></html>',
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
    browser = await chromium.launch({
        executablePath:
            process.env.WTM_CHROME ||
            '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
        headless: true,
    });
    const errors = [];
    const page = await browser.newPage({ viewport: { width: 1360, height: 860 } });
    page.on('pageerror', (e) => {
        errors.push(e.message);
        console.error('PAGE ERROR', e.message);
    });
    await page.goto(`http://127.0.0.1:${server.httpServer.address().port}/probe`);
    await page.waitForFunction(() => document.body.dataset.ready === 'yes');
    const shoot = async (name) => {
        if (shots)
            await page.screenshot({ path: `${shots}/${name}.png`, animations: 'disabled' });
    };
    const ghost = (key) =>
        page
            .locator('.c-worktree-tab--ghost')
            .filter({ hasText: key })
            .getByRole('treeitem');
    const tab = (key) =>
        page
            .locator('.c-worktree-tab:not(.c-worktree-tab--ghost)')
            .filter({ hasText: key })
            .getByRole('treeitem');
    const creates = () => page.evaluate(() => window.checkState.creates.map((c) => c.args));
    const settle = (index, how, value) =>
        page.evaluate(
            ([i, h, v]) => window.checkState.creates[i][h](v),
            [index, how, value],
        );

    async function startCreation(issue) {
        await page.getByRole('button', { name: /New Worktree/ }).click();
        await page.getByLabel('Jira issue').fill(issue);
        const create = page.getByRole('button', { name: 'Create worktree' });
        await page.waitForFunction(() =>
            [...document.querySelectorAll('button')].some(
                (b) => b.textContent?.trim() === 'Create worktree' && !b.disabled,
            ),
        );
        await create.click();
        await ghost(issue).waitFor();
    }

    await tab('ACME-1198').waitFor();

    // ── Create: a ghost appears where the worktree will land, and the run is tagged ──
    await startCreation('ACME-1234');
    const first = (await creates())[0];
    assert.ok(first.creation, 'the run is started with a creation id');
    await page.evaluate(
        (c) =>
            window.progress({
                kind: 'stage',
                id: 'worktree',
                label: 'git worktree add',
                index: 3,
                total: 5,
                creation: c,
            }),
        first.creation,
    );
    await ghost('ACME-1234').getByText('creating · 3/5 git worktree add').waitFor();
    await page.getByRole('heading', { name: 'git worktree add' }).waitFor();
    // Progress without this run's id is someone else's and must not move it.
    await page.evaluate(() =>
        window.progress({
            kind: 'stage',
            id: 'other',
            label: 'Somebody else',
            index: 1,
            total: 2,
        }),
    );
    await page.waitForTimeout(100);
    assert.equal(await ghost('ACME-1234').getByText('Somebody else').count(), 0);
    await shoot('ghost-creating');

    // ── Click away: the run carries on and its ghost keeps reporting ──
    await tab('ACME-1198').click();
    await page.getByText('Start a session in').waitFor();
    await page.evaluate(
        (c) =>
            window.progress({
                kind: 'stage',
                id: 'setup',
                label: 'Run setup',
                index: 5,
                total: 5,
                creation: c,
            }),
        first.creation,
    );
    await ghost('ACME-1234').getByText('creating · 5/5 Run setup').waitFor();
    // Git has made the worktree by now and a refresh lists it; the ghost stays its only tab.
    await page.evaluate(() => {
        const s = window.checkState;
        s.worktrees.push(
            s.wt('ACME-1234-export', 'task/ACME-1234-export', false, 'ACME-1234'),
        );
        window.focusWindow();
    });
    await page.waitForTimeout(300);
    assert.equal(
        await tab('ACME-1234').count(),
        0,
        'the real tab waits for its run to finish',
    );
    await shoot('ghost-away');

    // ── Back on the ghost: the same screen, further along ──
    await ghost('ACME-1234').click();
    await page.getByRole('heading', { name: 'Run setup' }).waitFor();
    await page.locator('.c-steps__item.is-done', { hasText: 'git worktree add' }).waitFor();
    assert.equal(await ghost('ACME-1234').getAttribute('aria-selected'), 'true');
    assert.equal(await tab('ACME-1198').getAttribute('aria-selected'), 'false');

    // ── It finishes while on screen: the worktree is gone to, with its record ──
    const made = await page.evaluate(() => window.checkState.worktrees.at(-1));
    await settle(0, 'resolve', { kind: 'created', worktree: made, setupSession: 'pty-1' });
    await tab('ACME-1234').getByText('created').waitFor();
    assert.equal(await ghost('ACME-1234').count(), 0, 'the ghost became the real tab');
    await tab('ACME-1234').getByText('new').waitFor();
    await page.getByText('Created ACME-1234-export').waitFor();
    await page.getByText('or start a session yourself').waitFor();
    await shoot('record-created');
    await page.getByRole('button', { name: 'Dismiss', exact: true }).click();
    await page.getByText('Created ACME-1234-export').waitFor({ state: 'detached' });
    assert.equal(
        await tab('ACME-1234').getByText('new').count(),
        0,
        'the pill goes with the record',
    );

    // ── Refused before git made anything: the ghost keeps the attempt and its form ──
    await startCreation('ACME-1240');
    await settle(1, 'reject', {
        kind: 'git',
        message: 'A branch named task/ACME-1240-export already exists.',
    });
    await ghost('ACME-1240').getByText('couldn’t create').waitFor();
    await page.getByText('A branch named task/ACME-1240-export already exists.').waitFor();
    assert.equal(
        await page.getByLabel('Jira issue').inputValue(),
        'ACME-1240',
        'the form comes back filled in',
    );
    // Its form re-plans like any other, so trying again is one click.
    await page.waitForFunction(() =>
        [...document.querySelectorAll('button')].some(
            (b) => b.textContent?.trim() === 'Try again' && !b.disabled,
        ),
    );
    await shoot('ghost-refused');
    await page.getByRole('button', { name: 'Discard this attempt' }).click();
    await ghost('ACME-1240').waitFor({ state: 'detached' });

    // ── Two at once, routed apart; one fails its setup while you are elsewhere ──
    await startCreation('ACME-1301');
    await startCreation('ACME-1302');
    const [, , a, b] = await creates();
    await page.evaluate(
        ([x]) =>
            window.progress({
                kind: 'stage',
                id: 'fetch',
                label: 'Fetch origin/develop',
                index: 2,
                total: 5,
                creation: x,
            }),
        [a.creation],
    );
    await ghost('ACME-1301').getByText('2/5 Fetch origin/develop').waitFor();
    assert.equal(
        await ghost('ACME-1302').getByText('Fetch origin/develop').count(),
        0,
        'each ghost has only its own progress',
    );
    await shoot('ghost-two');
    await tab('ACME-1198').click();
    const failedWt = await page.evaluate(() => {
        const s = window.checkState;
        const w = s.wt('ACME-1301-export', 'task/ACME-1301-export', false, 'ACME-1301');
        s.worktrees.push(w);
        return w;
    });
    await settle(2, 'resolve', {
        kind: 'setup_failed',
        worktree: failedWt,
        session: 'pty-2',
        outcome: { kind: 'failed', code: 2 },
        remedies: [],
    });
    await tab('ACME-1301').getByText('setup failed').waitFor();
    assert.equal(
        await tab('ACME-1198').getAttribute('aria-selected'),
        'true',
        'finishing elsewhere does not pull you to it',
    );
    await tab('ACME-1301').click();
    await page.getByText('Setup failed', { exact: true }).waitFor();
    await page.getByText('· setup exited with code 2').waitFor();
    await page.getByRole('button', { name: 'Re-run setup' }).waitFor();
    await page.getByRole('button', { name: 'Remove the worktree' }).waitFor();
    await shoot('record-failed');
    await ghost('ACME-1302').waitFor();

    assert.deepEqual(errors, []);
    console.log(
        'PASS: ghost on Create, progress routed by creation id, leaving and returning, the real tab held back until done, finishing into the worktree with its record and Dismiss, a refused attempt keeping its form and ×, two runs at once kept apart, and a failed setup found from its tab.',
    );
} finally {
    await browser?.close();
    await server.close();
}

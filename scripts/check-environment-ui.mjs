/**
 * Drive the real app's Environment tab and workspace header control with mocked IPC: every service
 * state (stopped, starting, healthy, unhealthy, failed), a port conflict naming its owner, a log
 * opened, Open preview asking Rust to verify the address before anything opens, a service left
 * running by the last launch, idle stop, and a destructive command's approval answered both in the
 * tab and in the focused Inbox.
 *
 * No Tauri, no processes, no ports. What it proves is the window's words and wiring against the
 * shapes `environment` and `pending_environment_command` return; supervision itself is
 * `src-tauri/tests/environment.rs`'s, against real processes.
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
const shots = process.env.WTM_SHOTS || join(tmpdir(), 'wtm-environment');
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
];
const id = (dir) => '/x/webapp-wt/' + dir;
const wt = (dir, branch) => ({
  id: id(dir), title: dir, subtitle: branch, path: id(dir), dirname: dir, branch, head: 'a1b2c3d4e5',
  isMain: false, isBare: false, locked: null, prunable: null, dirty: false, untracked: 0,
  staged: 0, ahead: 1, behind: 0, issueKey: null, badges: [], links: [], table: [], env: [],
  browserHome: null });
const worktrees = [
  { ...wt('webapp', 'main'), id: '/x/webapp', path: '/x/webapp', isMain: true },
  wt('export', 'feature/export'),
];
const service = (sid, state, more = {}) => ({ id: sid, label: sid, state, detail: null,
  ports: [{ port: 8123, problem: null }], health: 'http://localhost:8123/healthz',
  preview: 'http://localhost:8123/', detached: false, pid: null, leftRunning: false,
  conflicts: [], startedAt: null, updatedAt: Date.now() - 30_000, hasLog: false, ...more });
const run = (n, state, more = {}) => ({ id: n, flow: 40, command: 'reset', label: 'Reset the database',
  argv: ['sh', '-c', 'psql {{ env.DB_URL }} -f reset.sql'], state, exitCode: null, output: null,
  requestedAt: Date.now() - 10_000, finishedAt: null, ...more });
window.env = {
  project: '/x/webapp', worktree: id('export'), declared: true, problem: null, idleStopMinutes: null,
  services: [service('web', 'stopped', { hasLog: true })],
  resources: [{ id: 'db', label: 'shared database', isolation: 'shared' },
              { id: 'cache', label: 'cache', isolation: 'per_worktree' }],
  commands: [
    { id: 'reset', label: 'Reset the database', argv: ['sh', '-c', 'psql {{ env.DB_URL }} -f reset.sql'],
      destructive: true, resources: ['shared database'], needsApproval: true },
    { id: 'seed', label: 'Seed', argv: ['just', 'seed'], destructive: false, resources: [], needsApproval: false },
  ],
  runs: [],
};
window.pending = null;
window.inbox = [];
window.startRefusal = null;
window.calls = [];
mockIPC((cmd, args) => {
  const a = args ?? {};
  window.calls.push({ cmd, args: a });
  switch (cmd) {
    case 'get_pref': return localStorage.getItem(prefKey(a.key));
    case 'set_pref': localStorage.setItem(prefKey(a.key), a.value); return null;
    case 'list_projects': return structuredClone(projects);
    case 'list_worktrees': return structuredClone(worktrees);
    case 'sidebar_layout': return { groups: [], origins: {} };
    case 'list_agents': return [];
    case 'list_openers': return { openers: [], preferred: null };
    case 'usage_limits': return [];
    case 'browser_available': return { available: true, runtime: true, reason: null };
    case 'notification_permission': return 'granted';
    case 'dictation_status': return { available: false, recording: false, key: null, reason: 'harness' };
    case 'doctor': return { path: [], tools: [], problems: [] };
    case 'review_recipes': return [{ id: 'standard', label: 'Standard', roles: [] }];
    case 'latest_review': return null;
    case 'work_inbox': return structuredClone(window.inbox);
    case 'environment':
      return a.worktree === id('export') ? structuredClone(window.env)
        : { ...structuredClone(window.env), worktree: a.worktree, declared: false, services: [], resources: [], commands: [] };
    case 'start_services':
      if (window.startRefusal) throw window.startRefusal;
      window.env.services = window.env.services.map((s) => ({ ...s, state: 'starting', detail: null }));
      return structuredClone(window.env);
    case 'stop_services':
      window.env.services = window.env.services.map((s) => ({ ...s, state: 'stopped', pid: null, detail: 'stopped' }));
      return structuredClone(window.env);
    case 'restart_services':
    case 'check_service':
    case 'set_idle_stop':
      if (cmd === 'set_idle_stop') window.env.idleStopMinutes = a.minutes;
      return structuredClone(window.env);
    case 'verify_service_preview':
      return { url: 'http://localhost:8123/', status: 200, ms: 4 };
    case 'service_log':
      return { path: '/data/env/export-1a2b3c/' + a.service + '.log', truncated: false,
        text: 'listening on 8123\\nGET /healthz\\nTraceback (most recent call last):\\n  ValueError: no DB_URL' };
    case 'run_environment_command':
      window.env.runs = [run(7, 'awaiting_approval')];
      window.pending = { run: run(7, 'awaiting_approval'), project: '/x/webapp', worktree: id('export'),
        worktreeName: 'export', shared: ['\`shared database\`'] };
      return structuredClone(window.env);
    case 'pending_environment_command': return structuredClone(window.pending);
    case 'approve_environment_command':
      window.env.runs = [run(7, 'succeeded', { exitCode: 0, output: 'DROP TABLE\\nCREATE TABLE', finishedAt: Date.now() })];
      window.pending = null;
      return structuredClone(window.env.runs[0]);
    case 'decline_environment_command':
      window.env.runs = [run(7, 'declined', { finishedAt: Date.now() })];
      window.pending = null;
      return structuredClone(window.env.runs[0]);
    case 'open_browser':
      return { id: 'browser-1', project: a.projectId, worktree: a.worktreeId, url: a.url, title: '',
        loading: false, loadError: null, canGoBack: false, canGoForward: false };
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
const { environments } = await import('/src/lib/state/environments.svelte.ts');
window.harness = { emit, workspace, work, view, inbox, environments };
mount(App, { target: document.getElementById('app') });`;

const server = await createServer({
    configFile: false,
    envDir: false,
    logLevel: 'warn',
    plugins: [
        svelte(),
        {
            name: 'environment-probe',
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

const EXPORT = '/x/webapp-wt/export';

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
        await page.waitForFunction(() => window.harness?.workspace.worktrees.length > 0);
        await page.getByRole('region', { name: 'Work', exact: true }).waitFor();
        await page.evaluate((w) => window.harness.work.pick(w), EXPORT);
        await page.waitForFunction((w) => window.harness.environments.views[w], EXPORT);
        return page;
    }

    const shot = (page, name, scheme) =>
        page.screenshot({
            path: join(shots, `${name}-${scheme}.png`),
            animations: 'disabled',
        });
    const calls = (page, cmd) =>
        page.evaluate((c) => window.calls.filter((x) => x.cmd === c), cmd);
    const control = (page) => page.getByRole('group', { name: 'Environment' });
    const tab = (page) => page.getByRole('tab', { name: 'Environment' });
    /** Change the mocked environment, then announce it the way the supervisor does. */
    const announce = (page, change) =>
        page.evaluate(
            async ([body, w]) => {
                // eslint-disable-next-line no-new-func
                new Function('env', body)(window.env);
                await window.harness.emit('env:changed', {
                    project: '/x/webapp',
                    worktree: w,
                });
            },
            [change, EXPORT],
        );

    for (const scheme of ['light', 'dark']) {
        const page = await open(scheme);

        // ── Declared: the tab and the header control appear, stopped ─────────────────────────
        await tab(page).waitFor();
        assert.match(await control(page).textContent(), /Env: Stopped/);
        await page.evaluate((w) => window.harness.work.pick(w), '/x/webapp');
        await page.waitForFunction(() => window.harness.environments.views['/x/webapp']);
        assert.equal(await tab(page).count(), 0, 'no tab where nothing is declared');
        await page.evaluate((w) => window.harness.work.pick(w), EXPORT);
        await tab(page).click();
        await page.getByRole('list', { name: 'Services' }).waitFor();
        await page
            .getByText('Isolation · shared database: shared by every worktree')
            .waitFor();
        await shot(page, '01-stopped', scheme);

        // ── A port in use: refused, owner named, nothing ended ─────────────────────────────────
        await page.evaluate(() => {
            window.startRefusal = {
                kind: 'flow',
                message: '`web`: port 8123 is in use by `python3` (pid 4242)',
                detail: null,
            };
            window.env.services[0].detail = 'port 8123 is in use by `python3` (pid 4242)';
            window.env.services[0].conflicts = [
                {
                    port: 8123,
                    owners: [
                        { pid: 4242, command: 'python3', container: null, service: null },
                        {
                            pid: null,
                            command: 'postgres:16',
                            container: 'api-db-1',
                            service: null,
                        },
                    ],
                },
            ];
        });
        await control(page).getByRole('button', { name: 'Start', exact: true }).click();
        await page
            .getByText(
                'Port 8123 is in use by python3 (pid 4242) and container api-db-1 (postgres:16).',
            )
            .waitFor();
        await page.getByRole('alert').getByText('port 8123 is in use by').waitFor();
        assert.equal((await calls(page, 'start_services')).at(-1).args.service, null);
        await shot(page, '02-port-conflict', scheme);

        // ── Starting, then healthy: Open verifies before a pane is asked for ──────────────────
        await page.evaluate(() => {
            window.startRefusal = null;
            window.env.services[0].conflicts = [];
        });
        await page
            .getByRole('list', { name: 'Services' })
            .getByRole('button', { name: 'Start', exact: true })
            .click();
        await page.getByText('Starting', { exact: true }).waitFor();
        await announce(
            page,
            "env.services[0].state = 'healthy'; env.services[0].pid = 4243; env.services[0].detail = 'answered 200 in 3 ms';",
        );
        await page.getByText('answered 200 in 3 ms').waitFor();
        assert.match(await control(page).textContent(), /Env: Healthy/);
        await shot(page, '03-healthy', scheme);
        await control(page).getByRole('button', { name: 'Open preview' }).click();
        await page.waitForFunction(() =>
            window.calls.some((c) => c.cmd === 'verify_service_preview'),
        );
        const verify = (await calls(page, 'verify_service_preview')).at(-1);
        assert.deepEqual(verify.args, {
            project: '/x/webapp',
            worktree: EXPORT,
            service: 'web',
        });
        await page.waitForFunction(() =>
            window.calls.some((c) => c.cmd === 'open_browser'),
        );
        const opened = (await calls(page, 'open_browser')).at(-1);
        assert.equal(
            opened.args.url,
            'http://localhost:8123/',
            'only the verified address',
        );
        await page.waitForFunction(() => window.harness.view.current === 'worktree');
        await page.evaluate(() => window.harness.view.show('work'));
        await tab(page).click();

        // ── Unhealthy offers Check again; failed shows why, and its log ───────────────────────
        await announce(
            page,
            "env.services[0].state = 'unhealthy'; env.services[0].detail = 'no answer within 8 s: answered 503';",
        );
        await page.getByRole('button', { name: 'Check again' }).waitFor();
        await announce(
            page,
            "env.services[0].state = 'failed'; env.services[0].pid = null; env.services[0].detail = 'it exited with code 1';",
        );
        await page.getByText('it exited with code 1').waitFor();
        await page
            .getByRole('list', { name: 'Services' })
            .getByRole('button', { name: 'Logs', exact: true })
            .click();
        await page.getByText('ValueError: no DB_URL').waitFor();
        assert.match(await control(page).textContent(), /Env: Failed/);
        await shot(page, '04-failed-with-log', scheme);

        // ── Left running by the last launch ──────────────────────────────────────────────────
        await announce(
            page,
            "env.services[0].state = 'healthy'; env.services[0].leftRunning = true; env.services[0].detail = 'left running when wtm last closed; answered 200 in 2 ms';",
        );
        await page.getByText('Left running when wtm last closed: web.').waitFor();

        // ── A destructive command on a shared resource asks, and runs once on yes ─────────────
        await page.getByRole('button', { name: 'Ask to run' }).click();
        const card = page.getByRole('region', { name: 'Approval needed' });
        await card.waitFor();
        assert.match(await card.textContent(), /psql \{\{ env\.DB_URL \}\} -f reset\.sql/);
        assert.match(await card.textContent(), /every worktree shares/);
        await shot(page, '05-approval-in-tab', scheme);
        await card.getByRole('button', { name: 'Approve and run once' }).click();
        await page.getByText('Succeeded').waitFor();
        assert.equal((await calls(page, 'approve_environment_command')).at(-1).args.run, 7);
        assert.equal(await card.count(), 0, 'nothing waits once approved');

        // ── The same card in the focused Inbox, and Show lands on the tab ─────────────────────
        await page.getByRole('button', { name: 'Ask to run' }).click();
        await card.waitFor();
        await page.evaluate(async (w) => {
            window.inbox = [
                {
                    id: 31,
                    flow: 40,
                    kind: 'approval',
                    target: { kind: 'flow' },
                    title: 'Run Reset the database against `shared database`?',
                    body: 'It is destructive.',
                    state: 'open',
                    createdAt: Date.now(),
                    closedAt: null,
                    project: '/x/webapp',
                    worktree: w,
                    question: null,
                },
            ];
            await window.harness.emit('flow:inbox', window.inbox[0]);
            await window.harness.inbox.load();
            window.harness.inbox.chosen = 31;
            window.harness.work.inboxOpen = true;
        }, EXPORT);
        const inInbox = page.getByRole('region', { name: 'Approval needed' });
        await inInbox.waitFor();
        await shot(page, '06-approval-in-inbox', scheme);
        await page.getByRole('button', { name: 'Show the environment' }).click();
        await page.waitForFunction(
            () =>
                window.harness.work.tab === 'environment' && !window.harness.work.inboxOpen,
        );
        await page.getByRole('button', { name: 'Decline' }).click();
        await page.getByText('Declined; nothing ran').waitFor();

        // ── Idle stop is opt-in ───────────────────────────────────────────────────────────────
        await page.getByRole('combobox').selectOption('30');
        await page.waitForFunction(() =>
            window.calls.some((c) => c.cmd === 'set_idle_stop'),
        );
        assert.deepEqual((await calls(page, 'set_idle_stop')).at(-1).args, {
            project: '/x/webapp',
            minutes: 30,
        });
        await shot(page, '07-commands-and-idle', scheme);

        await page.context().close();
        console.log(`${scheme}: ok`);
    }
    console.log(`screenshots in ${shots}`);
} finally {
    await browser?.close();
    await server.close();
}

/**
 * Drive the real app shell with mocked IPC: adding a pull request to the Work view's Reviews queue,
 * reading what the forge said about it, and preparing its review worktree. No Tauri, no `gh`, no
 * network.
 *
 * What it proves, in order: a project whose workflow names no forge offers no way to add one and
 * asks the backend nothing beyond the queue; pasting a URL adds one row, opens it, and shows the
 * fork, maintainer edits, your permission, the requested reviewers, the checks and the base's
 * protection; the preview runs with "Prepare environment" off until it is ticked, and carries the
 * head that was shown; a refused Prepare says why, and the next one sends exactly what was
 * previewed; the same pull request pasted again by number is still one row; a refusal to add is
 * shown where it was typed; and picking a worktree leaves the pull request.
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
const shots = process.env.WTM_SHOTS || join(tmpdir(), 'wtm-review-queue');
mkdirSync(shots, { recursive: true });

/*
 * The page: the real `App` with the Work view on, after a mock of every command it calls. The
 * mocked backend keeps a queue, a worktree listing and one pull request, and answers the review
 * commands the way `reviews.rs` does: an add of a queued pull request returns the queued row, a
 * pull request on a repository with no remote is refused, and preparing adds a worktree to the
 * listing. `?forge=none` is a project whose workflow names no forge.
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
const worktrees = [wt('webapp', 'main'), wt('fix-date-picker', 'fix/date-picker')];
const forge = params.get('forge') === 'none' ? null : 'github';
const HEAD = '1156f977ff6d12d592bc152987b9d9b378f6718d';
const queue = [];
const pullRequest = {
  number: 4031, url: 'https://github.com/acme/webapp/pull/4031', title: 'Export units as CSV',
  author: 'sam-rivera', state: 'open', draft: false,
  head: { repo: { host: 'github.com', owner: 'sam-rivera', name: 'webapp' }, branch: 'main', sha: HEAD },
  base: { repo: { host: 'github.com', owner: 'acme', name: 'webapp' }, branch: 'main', sha: null },
  crossRepository: true, maintainerCanModify: false,
  requestedReviewers: [{ name: 'dana-lee', team: false }, { name: 'acme/frontend', team: true }],
  checks: { passed: 2, failed: 1, pending: 2, skipped: 1 },
};
const detail = (item) => ({
  item: structuredClone(item), pullRequest: structuredClone(pullRequest), checksState: 'failing',
  permission: 'read', canPush: false, baseIntegration: true,
  baseProtection: { branch: 'main', protected: true, rules: ['pull_request', 'required_status_checks'] },
  account: { host: 'github.com', login: 'dana-lee', version: '2.100.0' },
});
const fail = (kind, message) => { throw { kind, message, detail: null }; };
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
    case 'review_queue': return { forge, items: structuredClone(queue) };
    case 'add_review': {
      if (!forge) fail('forgeOff', 'no forge');
      if (a.input.includes('elsewhere'))
        fail('forge', '#9 is a pull request on elsewhere/webapp, and no remote of this project points there.');
      if (!/4031/.test(a.input)) fail('forge', 'pull request was not found, or this account cannot see it');
      let item = queue.find((i) => i.number === 4031);
      if (!item) { item = { id: 1, repo: 'acme/webapp', number: 4031, title: pullRequest.title, worktree: null, reviewingSince: null }; queue.push(item); }
      return detail(item);
    }
    case 'review_detail': return detail(queue.find((i) => i.id === a.itemId));
    case 'preview_review_worktree': {
      const item = queue.find((i) => i.id === a.args.itemId);
      const setup = a.args.prepareEnvironment
        ? { kind: 'run', argv: ['./bin/setup.sh', '/x/webapp-pr-4031'], cwd: '/x/webapp' }
        : { kind: 'skipped' };
      if (item.worktree)
        return { number: 4031, head: HEAD, fetch: null, headPresent: true,
          target: { kind: 'reuse', worktreeId: item.worktree, path: item.worktree, reason: 'head_commit', atHead: true },
          setup: { kind: 'not_applicable' }, preflight: [], warnings: [], canCreate: true };
      return { number: 4031, head: HEAD, fetch: { remote: 'origin', refspec: 'pull/4031/head' }, headPresent: false,
        target: { kind: 'create', directory: '/x/webapp-pr-4031', branch: a.args.checkout === 'branch' ? 'pr-4031' : null,
          gitArgv: ['git', '-c', 'core.hooksPath=/dev/null', 'worktree', 'add', ...(a.args.checkout === 'branch' ? ['--no-track', '-b', 'pr-4031'] : ['--detach']), '--', '/x/webapp-pr-4031', HEAD] },
        setup, preflight: [], warnings: [], canCreate: true };
    }
    case 'create_review_worktree': {
      if (window.failNextCreate) {
        window.failNextCreate = false;
        fail('preflight', 'Pull request #4031 moved from 1156f97 to 9e0d1c2 since it was shown.');
      }
      const item = queue.find((i) => i.id === a.args.itemId);
      worktrees.push(wt('webapp-pr-4031', null, { head: HEAD }));
      item.worktree = '/x/webapp-pr-4031';
      return { kind: 'created', worktreeId: '/x/webapp-pr-4031', path: '/x/webapp-pr-4031', setupSession: null };
    }
    default: return cmd.startsWith('list_') ? [] : null;
  }
}, { shouldMockEvents: true });
await import('@xterm/xterm/css/xterm.css');
await import('/src/styles/main.scss');
const { mount } = await import('svelte');
const { default: App } = await import('/src/App.svelte');
const { view } = await import('/src/lib/state/view.svelte.ts');
const { workspace } = await import('/src/lib/state/workspace.svelte.ts');
window.harness = { emit, view, workspace };
mount(App, { target: document.getElementById('app') });`;

const server = await createServer({
    configFile: false,
    envDir: false,
    logLevel: 'warn',
    plugins: [
        svelte(),
        {
            name: 'review-queue-probe',
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
    const reviewsSection = (page) => queue(page).getByRole('region', { name: /^Reviews/ });
    const field = (page) =>
        page.getByRole('textbox', { name: 'Pull request URL or number' });
    const workspaceTitle = (page) => page.locator('.c-work__title');
    const plan = (page) =>
        page.getByRole('definition').filter({ hasText: /git (fetch|worktree)/ });
    const prepareBox = (page) =>
        page.getByRole('checkbox', { name: 'Prepare environment' });

    async function add(page, text) {
        await field(page).fill(text);
        await field(page).press('Enter');
        await page.waitForFunction(
            () => !document.querySelector('.c-work-queue__add input:disabled'),
        );
    }

    // ── A project with no forge: no field, a reason, and no forge command at all ─────────────
    {
        const page = await open('?forge=none');
        await reviewsSection(page).getByText('names no forge').waitFor();
        assert.equal(await field(page).count(), 0, 'nothing to type into without a forge');
        assert.equal((await calls(page, 'add_review')).length, 0);
        assert.equal((await calls(page, 'review_detail')).length, 0);
        await shot(page, join(shots, 'no-forge-light.png'));
        await page.context().close();
    }

    for (const scheme of ['light', 'dark']) {
        const page = await open(`?theme=${scheme}`, scheme);

        // ── Paste a URL: one row, opened, with what the forge said ─────────────────────────
        assert.equal(await reviewsSection(page).locator('.c-work-row').count(), 0);
        await reviewsSection(page).getByText('Nothing to review yet.').waitFor();
        await add(page, 'https://github.com/acme/webapp/pull/4031/files');
        assert.deepEqual(
            (await calls(page, 'add_review')).map((c) => c.args),
            [
                {
                    projectId: '/x/webapp',
                    input: 'https://github.com/acme/webapp/pull/4031/files',
                },
            ],
        );
        const rows = reviewsSection(page).locator('.c-work-row');
        await rows.first().waitFor();
        assert.equal(await rows.count(), 1);
        assert.match(await rows.first().innerText(), /#4031 · Export units as CSV/);
        assert.match(await rows.first().innerText(), /No review worktree yet/);
        assert.equal(
            await rows.first().getAttribute('aria-current'),
            'true',
            'the new row is opened',
        );
        assert.equal(
            await field(page).inputValue(),
            '',
            'the field empties once it is queued',
        );

        await workspaceTitle(page).filter({ hasText: '#4031' }).waitFor();
        // Overview, which a pull request with no review worktree opens on: its facts and Prepare.
        const panel = page.getByRole('tabpanel', { name: 'Overview', exact: true });
        const text = await panel.innerText();
        for (const fact of [
            'sam-rivera:main',
            'Yes: maintainer edits off',
            'Read: you cannot push on acme/webapp',
            'dana-lee, acme/frontend (team)',
            '2 passed · 1 failed · 2 pending · 1 skipped',
            'Protected: pull request, required status checks',
        ])
            assert(text.includes(fact), `the panel says “${fact}”:\n${text}`);

        // ── The preview: environment off, the shown head pinned ────────────────────────────
        assert.equal(
            await prepareBox(page).isChecked(),
            false,
            'Prepare environment starts off',
        );
        await plan(page).filter({ hasText: 'git fetch origin pull/4031/head' }).waitFor();
        let previews = await calls(page, 'preview_review_worktree');
        assert.deepEqual(previews.at(-1).args.args, {
            itemId: 1,
            expectedHead: '1156f977ff6d12d592bc152987b9d9b378f6718d',
            checkout: 'detached',
            prepareEnvironment: false,
        });
        await panel.getByText('Skipped. Tick Prepare environment to run it.').waitFor();
        if (scheme === 'light')
            await shot(page, join(shots, `review-preview-${scheme}.png`));

        await prepareBox(page).check();
        await panel.getByText('./bin/setup.sh /x/webapp-pr-4031').waitFor();
        previews = await calls(page, 'preview_review_worktree');
        assert.equal(previews.at(-1).args.args.prepareEnvironment, true);
        if (scheme === 'dark')
            await shot(page, join(shots, `review-preview-environment-${scheme}.png`));
        await prepareBox(page).uncheck();
        await panel.getByText('Skipped. Tick Prepare environment to run it.').waitFor();

        // ── Prepare: a refusal is shown, then exactly what was previewed goes through ─────────
        await page.evaluate(() => (window.failNextCreate = true));
        await panel.getByRole('button', { name: 'Prepare review worktree' }).click();
        await panel.getByRole('alert').filter({ hasText: 'moved from' }).waitFor();
        await panel.getByRole('button', { name: 'Prepare review worktree' }).click();
        await panel.getByText('Created at /x/webapp-pr-4031.').waitFor();
        const created = await calls(page, 'create_review_worktree');
        assert.equal(created.length, 2);
        assert.equal(
            await panel.getByRole('alert').count(),
            0,
            'the refusal clears on success',
        );
        assert.deepEqual(created[1].args.args, {
            itemId: 1,
            expectedHead: '1156f977ff6d12d592bc152987b9d9b378f6718d',
            checkout: 'detached',
            prepareEnvironment: false,
            acknowledged: [],
        });
        await rows.first().getByText('Review worktree ready').waitFor();
        await panel.getByRole('button', { name: 'Open in Worktrees' }).waitFor();
        await shot(page, join(shots, `review-ready-${scheme}.png`));

        // ── The same pull request again, by number: still one row ──────────────────────────
        await add(page, '#4031');
        assert.equal((await calls(page, 'add_review')).length, 2);
        assert.equal(await rows.count(), 1, 'a second paste lands on the first row');

        // ── A refusal, where it was typed ──────────────────────────────────────────────────
        await add(page, 'https://github.com/elsewhere/webapp/pull/9');
        await reviewsSection(page)
            .getByRole('alert')
            .filter({ hasText: 'no remote' })
            .waitFor();
        assert.equal(await rows.count(), 1);
        assert.equal(
            await field(page).inputValue(),
            'https://github.com/elsewhere/webapp/pull/9',
            'a refused paste stays in the field to correct',
        );

        // ── A worktree row takes the workspace back ────────────────────────────────────────
        await queue(page)
            .getByRole('button', { name: /fix-date-picker|fix\/date-picker/ })
            .click();
        await workspaceTitle(page)
            .filter({ hasText: /date-picker/ })
            .waitFor();
        assert.equal(await rows.first().getAttribute('aria-current'), null);

        await page.context().close();
    }

    console.log(`ok — screenshots in ${shots}`);
} finally {
    await browser?.close();
    await server.close();
}

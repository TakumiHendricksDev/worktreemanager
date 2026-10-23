/**
 * Drive Settings › Recipes with mocked IPC: the real `App`, the real components, and a small
 * stateful stand-in for the four recipe commands. No Tauri, no config file, no CLI.
 *
 * What it proves, in order: with the Work view off there is no Recipes section; with it on the
 * section draws the approved board — the recipe picker, where the workflow is saved, the
 * enforced-profile notice, each stage's role cards with agent, skills, knowledge, paths, limits and
 * "if limited", and the roles that run by themselves; Check recipe validates the draft and asks for
 * the providers' skills, marking each skill listed or not and the role that cannot start, whose
 * missing skill one click removes; Edit role refuses a field validation refuses, under that field,
 * and applies when fixed; moving roles earlier and later crosses stages, an emptied stage says so
 * and goes, and Save sends the whole draft with the revision it read; a stage validation refuses is
 * marked under that stage and nothing is saved; Duplicate makes a named copy; deleting a role the
 * planner names is refused with the reason, and one a fallback names is taken out of it too; a save
 * against a changed file offers Reload; with nothing declared yet, a first recipe and role are made
 * from nothing and Save asks where to keep them, the local layer first; and opening the editor the
 * way the Reviews tab's Move role to Codex does lands on the role with Codex chosen and its Claude
 * model cleared, ready to be checked and saved.
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
const shots = process.env.WTM_SHOTS || join(tmpdir(), 'wtm-recipes');
mkdirSync(shots, { recursive: true });

/*
 * The page: the real `App` after a mock of every command it calls on the way up, plus the recipe
 * commands, which keep the "file" in memory, validate the few rules this script exercises the way
 * `WorkflowSpec::validate` words them, and refuse a save made from an older revision.
 * `?home=none` starts with nothing declared.
 */
const entry = `
import { emit } from '@tauri-apps/api/event';
import { mockIPC, mockWindows } from '@tauri-apps/api/mocks';
const params = new URLSearchParams(location.search);
const prefKey = (k) => 'harness.pref.' + k;
if (localStorage.getItem(prefKey('ui.notify')) === null) localStorage.setItem(prefKey('ui.notify'), 'off');
if (params.get('theme')) localStorage.setItem(prefKey('ui.theme'), params.get('theme'));
if (params.get('work')) localStorage.setItem(prefKey('ui.work_view'), params.get('work'));
document.documentElement.dataset.theme = localStorage.getItem(prefKey('ui.theme')) ?? 'light';
document.documentElement.dataset.palette = 'pine';
document.documentElement.dataset.platform = 'macos';
mockWindows('main');
const project = { id: '/x/webapp', name: 'webapp', root: '/x/webapp', usable: true, problem: null, trust: null };
const worktrees = [{
  id: '/x/webapp', title: 'webapp', subtitle: 'main', path: '/x/webapp', dirname: 'webapp', branch: 'main',
  head: 'a1b2c3d4e5', isMain: true, isBare: false, locked: null, prunable: null, dirty: false, untracked: 0,
  staged: 0, ahead: 0, behind: 0, issueKey: null, badges: [], links: [], table: [], env: [], browserHome: null }];

const role = (provider, objective, extra = {}) => ({
  objective, instructions: null, provider, model: null, effort: null, inputs: ['diff', 'rules', 'instructions'],
  output: 'findings', profile: 'reviewer', workspace: null, timeout_ms: 15 * 60000, retries: 1, fallback: 'ask', ...extra });
const none = params.get('home') === 'none';
let saved = none ? { role: {}, recipe: {} } : {
  role: {
    correctness: role('codex', 'Ticket coverage, behaviour, edge cases and regressions.', { model: 'gpt-6', effort: 'high', skills: ['/team-review'] }),
    conventions: role('claude', 'Unneeded architecture, plan deviations and rule violations.', { model: 'opus', effort: 'xhigh', skills: ['/team-review', '/commit'], fallback: 'conventions-codex' }),
    'conventions-codex': role('codex', 'Conventions, on Codex.', { model: 'gpt-6', skills: ['/team-review'] }),
    summary: role('claude', 'Summarise what the first stage found.', { inputs: ['diff', 'findings'], paths: ['src/**/*.vue', 'src/**/*.scss'], timeout_ms: 90000 }),
    planner: role('claude', 'Investigate the issue and plan it.', { output: 'plan', inputs: ['issue', 'instructions'] }),
    implementer: role('codex', 'Implement the approved plan.', { output: 'handoff', profile: 'writer', inputs: ['issue', 'plan'] }),
  },
  recipe: {
    standard: { label: 'Standard', stage: [{ roles: ['correctness', 'conventions'] }, { roles: ['summary'] }], concurrency: 3 },
    quick: { label: 'Quick', stage: [{ roles: ['correctness'] }], concurrency: null },
  },
};
let home = none ? null : 'local';
let revision = 1;
window.refuseNextSave = null;
const places = [
  { home: 'defaults', header: '[defaults.workflow]', path: '/Users/someone/.config/wtm/config.toml' },
  { home: 'project', header: '[projects."/x/webapp".config.workflow]', path: '/Users/someone/.config/wtm/config.toml' },
  { home: 'local', header: '[workflow]', path: '/x/webapp/.git/wtm.local.toml' },
];
const view = () => structuredClone({
  file: { home, declaring: home ? [home] : [], roleHomes: Object.fromEntries(Object.keys(saved.role).map((r) => [r, home ? [home] : []])),
    recipeHomes: {}, userPath: places[0].path, localPath: places[2].path, rootKey: '/x/webapp', revision: String(revision) },
  places,
  workflow: { ...saved, planner: none ? null : 'planner', designer: null,
    implement: none ? null : { writer: 'implementer', recipe: 'standard', checks: null, fix_rounds: 2, fix_attempts: 1 },
    checks: [{ id: 'unit', label: 'Unit tests' }] },
});
const refuse = (detail, message) => { throw { kind: 'recipes', message, detail }; };
const invalid = (key, message) => refuse({ kind: 'invalid', key, message }, message);
const validate = (d) => {
  for (const [id, r] of Object.entries(d.role)) {
    for (const s of r.skills ?? []) if (/\\s/.test(s.trim())) invalid('role.' + id + '.skills', '\`' + s + '\` is not a skill name');
    if (r.retries > 3) invalid('role.' + id + '.retries', 'at most 3: each retry is a whole turn against your usage window');
  }
  for (const [id, r] of Object.entries(d.recipe)) {
    r.stage.forEach((stage, i) => {
      if (stage.roles.length === 0) invalid('recipe.' + id + '.stage[' + i + '].roles', 'a stage needs at least one role');
      for (const name of stage.roles) if (!(name in d.role)) invalid('recipe.' + id + '.stage[' + i + '].roles', 'there is no role \`' + name + '\`');
    });
  }
};
window.calls = [];
mockIPC((cmd, args) => {
  const a = args ?? {};
  window.calls.push({ cmd, args: structuredClone(a) });
  switch (cmd) {
    case 'get_pref': return localStorage.getItem(prefKey(a.key));
    case 'set_pref': localStorage.setItem(prefKey(a.key), a.value); return null;
    case 'set_work_view': localStorage.setItem(prefKey('ui.work_view'), a.enabled ? 'on' : 'off'); return null;
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
    case 'recipe_settings': return view();
    case 'check_recipes': validate(a.draft); return null;
    case 'save_recipes': {
      if (window.refuseNextSave === 'stale') { window.refuseNextSave = null; refuse({ kind: 'stale' }, 'the config changed on disk since Settings read it; reload to see it, then edit again'); }
      if (a.revision !== String(revision)) refuse({ kind: 'stale' }, 'the config changed on disk since Settings read it; reload to see it, then edit again');
      if (!home && !a.home) refuse({ kind: 'no_home' }, 'no file declares this project\\'s workflow yet; choose where to keep it');
      validate(a.draft);
      saved = structuredClone(a.draft);
      home = home ?? a.home;
      revision += 1;
      return view();
    }
    case 'check_recipe_skills': return {
      roles: a.draft.recipe[a.recipe].stage.flatMap((s) => s.roles),
      providers: [
        { provider: 'claude', listed: ['commit', 'other-skill'], problem: null },
        { provider: 'codex', listed: ['team-review'], problem: null },
      ],
      problems: Object.values(a.draft.recipe[a.recipe].stage).flatMap((s) => s.roles)
        .filter((r) => a.draft.role[r]?.provider === 'claude' && (a.draft.role[r].skills ?? []).length > 0)
        .map((r) => ({ kind: 'missing', role: r, provider: 'claude', skill: 'team-review' })) };
    default: return cmd.startsWith('list_') ? [] : null;
  }
}, { shouldMockEvents: true });
await import('@xterm/xterm/css/xterm.css');
await import('/src/styles/main.scss');
const { mount } = await import('svelte');
const { default: App } = await import('/src/App.svelte');
const { view: views } = await import('/src/lib/state/view.svelte.ts');
const { workspace } = await import('/src/lib/state/workspace.svelte.ts');
const { settingsRequest } = await import('/src/lib/state/settings.svelte.ts');
window.harness = { emit, view: views, workspace, settingsRequest, saved: () => structuredClone(saved) };
mount(App, { target: document.getElementById('app') });`;

const server = await createServer({
    configFile: false,
    envDir: false,
    logLevel: 'warn',
    plugins: [
        svelte(),
        {
            name: 'recipes-probe',
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

    async function open(query, colorScheme = 'light', width = 1440, height = 980) {
        const context = await browser.newContext({
            viewport: { width, height },
            colorScheme,
            locale: 'en-US',
            timezoneId: 'UTC',
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
        await page.waitForFunction(() => !document.body.textContent.includes('Starting…'));
        return page;
    }

    const shot = (page, path) => page.screenshot({ path, animations: 'disabled' });
    const calls = (page, cmd) =>
        page.evaluate(
            (c) => window.calls.filter((x) => x.cmd === c).map((x) => x.args),
            cmd,
        );
    const settings = (page) => page.getByRole('dialog', { name: 'Settings' });
    const card = (page, id) => settings(page).locator(`#recipe-role-${id}`);
    const stageTitles = (page) =>
        settings(page).locator('.c-recipe-stage__title').allTextContents();
    const stageRoles = (page) =>
        settings(page)
            .locator('.c-recipe-stage')
            .evaluateAll((stages) =>
                stages
                    .filter((s) =>
                        /^Stage/.test(
                            s.querySelector('.c-recipe-stage__title')?.textContent ?? '',
                        ),
                    )
                    .map((s) =>
                        [...s.querySelectorAll('.c-recipe-role__title')].map((t) =>
                            t.textContent.trim(),
                        ),
                    ),
            );
    const roleDialog = (page, title) => page.getByRole('dialog', { name: title });

    async function openRecipes(page) {
        await page.getByRole('button', { name: 'Settings', exact: true }).click();
        await settings(page).getByRole('tab', { name: 'Recipes' }).click();
        await settings(page).getByText('Review recipes', { exact: true }).waitFor();
    }

    // ── Off: no Recipes section ───────────────────────────────────────────────────────────────
    {
        const page = await open('?work=off');
        await page.getByRole('button', { name: 'Settings', exact: true }).click();
        await settings(page).getByRole('tab', { name: 'General' }).waitFor();
        assert.equal(
            await settings(page).getByRole('tab', { name: 'Recipes' }).count(),
            0,
            'Recipes belongs to the Work view and is hidden while it is off',
        );
        await page.context().close();
    }

    // ── On: the approved board, light and dark ───────────────────────────────────────────────
    for (const scheme of ['light', 'dark']) {
        const page = await open(`?work=on&theme=${scheme}`, scheme);
        await openRecipes(page);
        assert.deepEqual(
            await settings(page).getByRole('tab').allTextContents(),
            ['Appearance', 'General', 'Notifications', 'Recipes', 'Knowledge', 'Advanced'],
            'Recipes sits beside Knowledge',
        );
        assert.deepEqual(
            await settings(page).locator('.c-recipes__picker option').allTextContents(),
            ['Standard (implementation review)', 'Quick'],
        );
        const where = await settings(page).locator('.c-recipes__where').textContent();
        assert(where.includes('[workflow] in /x/webapp/.git/wtm.local.toml'), where);
        assert(
            (await settings(page).locator('.c-recipes__notice').textContent()).includes(
                'Every role runs as a read-only reviewer.',
            ),
        );
        assert.deepEqual(await stageTitles(page), [
            'Stage 1 · in parallel',
            'Stage 2 · after stage 1',
            'Roles that run by themselves',
            'Roles this recipe does not run',
        ]);
        const correctness = await card(page, 'correctness').textContent();
        for (const expected of [
            'Codex · gpt-6 · high',
            '/team-review',
            'not checked yet',
            'Your rules on touched paths + the repository’s instructions',
            'All changed files',
            '15 min · 1 retry',
            'Pause and ask me',
        ])
            assert(correctness.includes(expected), `${expected} in ${correctness}`);
        assert(
            (await card(page, 'conventions').textContent()).includes(
                'Switch to conventions-codex on Codex · gpt-6, then ask me',
            ),
        );
        assert(
            (await card(page, 'summary').textContent()).includes(
                'src/**/*.vue, src/**/*.scss',
            ),
        );
        assert.deepEqual(
            await settings(page)
                .locator('.c-recipe-stage')
                .nth(2)
                .locator('.c-recipe-role__title')
                .allTextContents()
                .then((t) => t.map((x) => x.trim())),
            ['Planner · planner', 'Implementer · implementer'],
        );
        await shot(page, join(shots, `recipes-${scheme}.png`));

        // ── Check recipe: validation, then each provider's own skills ───────────────────────
        await settings(page).getByRole('button', { name: 'Check recipe' }).click();
        await card(page, 'conventions').getByText('Needs a fix').waitFor();
        assert.equal((await calls(page, 'check_recipes')).length, 1);
        assert.deepEqual(
            (await calls(page, 'check_recipe_skills')).map((c) => c.recipe),
            ['standard'],
        );
        assert(
            (await card(page, 'conventions').textContent()).includes(
                '✗ not listed by Claude here',
            ),
        );
        assert(
            (await card(page, 'correctness').textContent()).includes('✓ listed by Codex'),
        );
        assert(
            (await settings(page).locator('.c-banner').textContent()).includes(
                'One role would not start',
            ),
        );
        await shot(page, join(shots, `checked-${scheme}.png`));
        await page.context().close();
    }

    // ── Editing: fields, reordering, saving, refusals ────────────────────────────────────────
    {
        const page = await open('?work=on');
        await openRecipes(page);
        await settings(page).getByRole('button', { name: 'Check recipe' }).click();
        await card(page, 'conventions').getByText('Needs a fix').waitFor();

        // The missing skill, removed in one click; the one Claude lists stays.
        assert(
            (await card(page, 'conventions').textContent()).includes('✓ listed by Claude'),
        );
        await card(page, 'conventions')
            .getByRole('button', { name: 'Remove skill from this role' })
            .click();
        const kept = await card(page, 'conventions').textContent();
        assert(kept.includes('/commit') && !kept.includes('/team-review'), kept);

        // A timeout set in seconds survives an edit that does not touch it.
        assert((await card(page, 'summary').textContent()).includes('1.5 min · 1 retry'));
        await card(page, 'summary').getByRole('button', { name: 'Edit role' }).click();
        const summary = roleDialog(page, 'Edit role summary');
        assert.equal(await summary.getByLabel('Timeout, minutes').inputValue(), '1.5');
        await summary.getByLabel('Objective').fill('Summarise the first stage.');
        await summary.getByRole('button', { name: 'Apply' }).click();
        await summary.waitFor({ state: 'detached' });

        // A field validation refuses is shown under that field, and the dialog stays.
        await card(page, 'correctness').getByRole('button', { name: 'Edit role' }).click();
        const edit = roleDialog(page, 'Edit role correctness');
        await edit.getByLabel('Skills').fill('team review');
        await edit.getByLabel('Model').fill('gpt-6-high');
        await edit.getByRole('button', { name: 'Apply' }).click();
        await edit
            .locator('.c-field', { hasText: 'Skills' })
            .getByText('is not a skill name', { exact: false })
            .waitFor();
        await shot(page, join(shots, 'role-refused.png'));
        await edit.getByLabel('Skills').fill('/team-review');
        await edit.getByLabel('Retries').selectOption('2');
        await edit.getByRole('button', { name: 'Apply' }).click();
        await edit.waitFor({ state: 'detached' });
        assert(
            (await card(page, 'correctness').textContent()).includes(
                'Codex · gpt-6-high · high',
            ) &&
                (await card(page, 'correctness').textContent()).includes(
                    '15 min · 2 retries',
                ),
        );
        await card(page, 'correctness').getByText('Edited').waitFor();

        // Earlier and later read the stages as one list.
        await settings(page).getByRole('button', { name: 'Move summary earlier' }).click();
        assert.deepEqual(await stageRoles(page), [
            ['correctness', 'conventions', 'summary'],
            [],
        ]);
        await settings(page).getByText('No role in this stage yet').waitFor();
        await settings(page).getByRole('button', { name: 'Remove stage 2' }).click();
        assert.deepEqual(await stageRoles(page), [
            ['correctness', 'conventions', 'summary'],
        ]);
        await settings(page)
            .getByRole('button', { name: 'Move correctness later' })
            .click();
        assert.deepEqual(await stageRoles(page), [
            ['conventions', 'correctness', 'summary'],
        ]);

        await settings(page).getByRole('button', { name: 'Save' }).click();
        await settings(page)
            .locator('.c-banner', { hasText: 'Runs started from now on' })
            .waitFor();
        const sent = (await calls(page, 'save_recipes')).at(-1);
        assert.equal(sent.revision, '1');
        assert.equal(
            sent.home,
            null,
            'a project that declares a workflow is written where it is',
        );
        assert.deepEqual(sent.draft.recipe.standard.stage, [
            { roles: ['conventions', 'correctness', 'summary'] },
        ]);
        assert.equal(sent.draft.role.correctness.model, 'gpt-6-high');
        assert.equal(sent.draft.role.correctness.retries, 2);
        assert.deepEqual(sent.draft.role.conventions.skills, ['/commit']);
        assert.equal(
            sent.draft.role.summary.timeout_ms,
            90000,
            'not rounded to a whole minute',
        );
        assert(
            (await settings(page).locator('.c-banner').textContent()).includes(
                'runs already under way keep the recipe they started with',
            ),
        );

        // A stage validation refuses is marked under that stage, and nothing is saved.
        await settings(page).getByRole('button', { name: 'Add a role or stage' }).click();
        await settings(page)
            .getByLabel('Role', { exact: true })
            .selectOption('conventions-codex');
        await settings(page)
            .getByLabel('Into')
            .selectOption({ label: 'A new stage after the last' });
        await settings(page).getByRole('button', { name: 'Add', exact: true }).click();
        assert.deepEqual(await stageRoles(page), [
            ['conventions', 'correctness', 'summary'],
            ['conventions-codex'],
        ]);
        await settings(page)
            .getByRole('button', { name: 'Remove conventions-codex from this recipe' })
            .click();
        await settings(page).getByRole('button', { name: 'Save' }).click();
        await settings(page)
            .locator('.c-recipe-stage__error')
            .getByText('a stage needs at least one role')
            .waitFor();
        assert.equal((await calls(page, 'save_recipes')).length, 2);
        assert.equal(
            (await page.evaluate(() => window.harness.saved())).recipe.standard.stage
                .length,
            1,
            'the refused save changed nothing',
        );
        await shot(page, join(shots, 'stage-refused.png'));
        await settings(page).getByRole('button', { name: 'Remove stage 2' }).click();

        // Duplicate: a named copy, picked.
        await settings(page).getByRole('button', { name: 'Duplicate' }).click();
        const dup = page.getByRole('dialog', { name: 'Duplicate recipe' });
        await dup.getByLabel('Name').fill('Thorough');
        await dup.getByRole('button', { name: 'Duplicate' }).click();
        assert.equal(
            await settings(page).locator('.c-recipes__picker select').inputValue(),
            'thorough',
        );

        // Deleting the planner's role is refused, with the reason.
        await card(page, 'planner').getByRole('button', { name: 'Edit role' }).click();
        const planner = roleDialog(page, 'Edit role planner');
        await planner.getByRole('button', { name: 'Delete role…' }).click();
        await planner.getByText('it is the planner', { exact: false }).waitFor();
        assert.equal(await planner.getByRole('button', { name: 'Delete role' }).count(), 0);
        await planner.getByRole('button', { name: 'Cancel' }).click();

        // Deleting a fallback's role takes it out of the fallback too.
        await settings(page)
            .locator('.c-recipes__other', { hasText: 'conventions-codex' })
            .getByRole('button', { name: 'Edit role' })
            .click();
        const spare = roleDialog(page, 'Edit role conventions-codex');
        await spare.getByRole('button', { name: 'Delete role…' }).click();
        await spare
            .getByText('conventions’s fallback'.replace('’', "'"), { exact: false })
            .waitFor();
        await spare.getByRole('button', { name: 'Delete role', exact: true }).click();
        assert(
            (await card(page, 'conventions').textContent()).includes('Pause and ask me'),
            'its fallback is gone with it',
        );

        // A file changed meanwhile: the save is refused, and Reload is offered.
        await page.evaluate(() => (window.refuseNextSave = 'stale'));
        await settings(page).getByRole('button', { name: 'Save' }).click();
        await settings(page).getByRole('button', { name: 'Reload' }).waitFor();
        await page.context().close();
    }

    // ── Nothing declared yet: a first recipe from nothing, and Save asks where ───────────────
    {
        const page = await open('?work=on&home=none');
        await openRecipes(page);
        await settings(page)
            .getByText('Nothing declares a workflow for webapp yet')
            .waitFor();
        await settings(page).getByRole('button', { name: 'New recipe' }).click();
        const named = page.getByRole('dialog', { name: 'New recipe' });
        await named.getByLabel('Name').fill('Quick');
        await named.getByRole('button', { name: 'Make recipe' }).click();
        const fresh = roleDialog(page, 'New role');
        await fresh.getByLabel('Role id').fill('correctness');
        await fresh.getByLabel('Objective').fill('Find bugs the change introduces');
        await fresh.getByLabel('Provider').selectOption('codex');
        await fresh.getByRole('button', { name: 'Apply' }).click();
        await fresh.waitFor({ state: 'detached' });
        assert.equal(
            (await calls(page, 'check_recipes')).at(-1).home,
            'local',
            'with nowhere chosen yet, it is checked as the local layer would hold it',
        );
        assert.deepEqual(await stageRoles(page), [['correctness']]);
        await settings(page).getByRole('button', { name: 'Save' }).click();
        const where = page.getByRole('dialog', {
            name: 'Where should these recipes live?',
        });
        await where.waitFor();
        assert(
            await where
                .getByRole('radio', { name: /This repository, on this Mac/ })
                .isChecked(),
        );
        await shot(page, join(shots, 'choose-home.png'));
        await where.getByRole('button', { name: 'Save here' }).click();
        await settings(page)
            .locator('.c-banner', { hasText: 'Runs started from now on' })
            .waitFor();
        assert.equal((await calls(page, 'save_recipes')).at(-1).home, 'local');
        assert.deepEqual(
            (await page.evaluate(() => window.harness.saved())).recipe.quick.stage,
            [{ roles: ['correctness'] }],
        );
        await page.context().close();
    }

    // ── Opened as the Reviews tab's Edit recipe opens it: only the reported skill is missing ──
    {
        const page = await open('?work=on');
        await page.evaluate(() =>
            window.harness.settingsRequest.showRecipes({
                project: '/x/webapp',
                recipe: 'standard',
                role: 'conventions',
                provider: null,
                problem: {
                    kind: 'missing',
                    role: 'conventions',
                    provider: 'claude',
                    skill: 'team-review',
                },
            }),
        );
        const editing = roleDialog(page, 'Edit role conventions');
        await editing.waitFor();
        assert.equal(await editing.getByLabel('Provider').inputValue(), 'claude');
        await editing.getByRole('button', { name: 'Cancel' }).click();
        const marks = await card(page, 'conventions')
            .locator('.c-recipe-role__skill')
            .allTextContents();
        assert.deepEqual(
            marks.map((m) => m.replace(/\s+/g, ' ').trim()),
            ['/team-review ✗ not listed by Claude here', '/commit not checked yet'],
            'a run stops at the first missing skill, so the others are not known',
        );
        await card(page, 'conventions')
            .getByRole('button', { name: 'Remove skill from this role' })
            .click();
        const kept = await card(page, 'conventions').textContent();
        assert(kept.includes('/commit') && !kept.includes('/team-review'), kept);
        await page.context().close();
    }

    // ── Opened as the Reviews tab's Move role to Codex opens it ──────────────────────────────
    {
        const page = await open('?work=on');
        await page.evaluate(() =>
            window.harness.settingsRequest.showRecipes({
                project: '/x/webapp',
                recipe: 'standard',
                role: 'conventions',
                provider: 'codex',
                problem: {
                    kind: 'missing',
                    role: 'conventions',
                    provider: 'claude',
                    skill: 'team-review',
                },
            }),
        );
        const moving = roleDialog(page, 'Edit role conventions');
        await moving.waitFor();
        assert.equal(await moving.getByLabel('Provider').inputValue(), 'codex');
        assert.equal(
            await moving.getByLabel('Model').inputValue(),
            '',
            'Claude’s model is not carried over',
        );
        assert((await moving.textContent()).includes('Moved from Claude to Codex'));
        await shot(page, join(shots, 'move-to-codex.png'));
        await moving.getByRole('button', { name: 'Apply' }).click();
        await moving.waitFor({ state: 'detached' });
        const checked = (await calls(page, 'check_recipes')).at(-1).draft.role.conventions;
        assert.equal(checked.provider, 'codex');
        assert.equal(checked.model, null);
        assert.equal(
            await settings(page)
                .getByRole('tab', { name: 'Recipes' })
                .getAttribute('aria-selected'),
            'true',
        );
        await settings(page).getByRole('button', { name: 'Save' }).click();
        await settings(page)
            .locator('.c-banner', { hasText: 'Runs started from now on' })
            .waitFor();
        assert.equal(
            (await page.evaluate(() => window.harness.saved())).role.conventions.provider,
            'codex',
        );
        // Closing and reopening from the menu lands where it always did, not on the role.
        await settings(page).getByRole('button', { name: 'Done' }).click();
        await page.getByRole('button', { name: 'Settings', exact: true }).click();
        assert.equal(
            await settings(page)
                .getByRole('tab', { name: 'Appearance' })
                .getAttribute('aria-selected'),
            'true',
        );
        await page.context().close();
    }

    // ── Narrow and dark: the cards wrap, and nothing runs off the dialog ─────────────────────
    {
        const page = await open('?work=on&theme=dark', 'dark', 860, 760);
        await openRecipes(page);
        const dialog = await settings(page).boundingBox();
        for (const id of ['correctness', 'conventions', 'summary']) {
            const box = await card(page, id).boundingBox();
            assert(
                box.x >= dialog.x && box.x + box.width <= dialog.x + dialog.width + 0.5,
                `${id}'s card stays inside the dialog`,
            );
        }
        await shot(page, join(shots, 'recipes-narrow-dark.png'));
        await page.context().close();
    }

    console.log(`ok — screenshots in ${shots}`);
} finally {
    await browser?.close();
    await server.close();
}

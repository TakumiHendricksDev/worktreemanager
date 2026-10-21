/**
 * Drive Settings › Knowledge and the Save as rule dialog with mocked IPC: the real `App`, the real
 * components, and a small stateful stand-in for the six knowledge commands. No Tauri, no database,
 * no CLI.
 *
 * What it proves, in order: with the Work view off there is no Knowledge section; with it on the
 * section lists one project's rules under the design's four filters, with an agent's proposal
 * beneath the active list; approving, editing, turning off and on, and replacing each send the
 * command they should and land the pane on the rule they changed; New rule saves a draft with its
 * scope split into areas and globs, and a glob the backend refuses is shown under Files with the
 * dialog left open; Edit first approves a proposal as edited; the instruction sources are listed
 * with Propose to repository disabled; Save as rule from a finding prefills from the finding and
 * sends it; and turning Work off with the section open falls back to General.
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
const shots = process.env.WTM_SHOTS || join(tmpdir(), 'wtm-knowledge');
mkdirSync(shots, { recursive: true });

/*
 * The page: the real `App` after a mock of every command it calls on the way up, plus the rule
 * commands, which keep their rules in memory and apply changes the way the domain does for the
 * cases this script exercises. `window.harness.openFinding` mounts the real Save as rule dialog for
 * a finding, since the Reviews tab that will open it from a finding does not exist yet.
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

const day = (d) => Date.UTC(2026, 9, d, 12);
const draft = (kind, text, rationale, areas, paths, examples = [], references = []) =>
  ({ kind, text, rationale, scope: { areas, paths }, examples, references });
const rev = (number, d, at) => ({ number, draft: d, at });
const rule = (id, extra) => ({ id, project: '/x/webapp', origin: 'human', source: { kind: 'written' },
  status: 'active', approval: 'approved', supersededBy: null, revisions: [], updatedAt: day(12), usage: null,
  sourceSubject: null, ...extra });
const rules = [
  rule(2, { status: 'superseded', supersededBy: 14, revisions: [rev(1, draft('guidance', 'Prefer AppDialog', '', ['frontend'], []), day(2))] }),
  rule(3, { revisions: [rev(1, draft('required', "Validate dates in the record's own timezone", 'A server-local date moves records near midnight.', ['backend'], ['api/**/*.py']), day(3))],
    usage: { reviews: 9, lastAt: day(19), lastSubject: 'APP-8690' } }),
  rule(5, { status: 'superseded', supersededBy: 22, revisions: [rev(1, draft('guidance', 'Ask before adding packages', '', [], []), day(5))] }),
  rule(9, { status: 'disabled', revisions: [rev(1, draft('guidance', 'Prefer class components', '', ['frontend'], []), day(9))] }),
  rule(14, { source: { kind: 'finding', flow: 7, finding: 31, location: { path: 'src/exports/ExportModal.vue', startLine: 12, endLine: 12 } },
    sourceSubject: 'APP-8642', updatedAt: day(15), usage: { reviews: 6, lastAt: day(20), lastSubject: 'APP-8701' },
    revisions: [
      rev(1, draft('required', 'Use AppDialog instead of BaseModal.', '', ['frontend'], ['src/**/*.vue']), day(12)),
      rev(2, draft('required', 'Use AppDialog for confirm and cancel flows',
        'AppDialog carries the focus trap, Escape handling and button order our confirmations need. BaseModal is fine for read-only content.',
        ['frontend'], ['src/**/*.vue'],
        [{ stance: 'prefer', text: 'src/components/AppDialog.vue' }, { stance: 'avoid', text: 'src/components/BaseModal.vue for confirmations' }]), day(15)),
    ] }),
  rule(21, { revisions: [rev(1, draft('guidance', 'Export file names use kebab-case', '', ['exports'], []), day(14))],
    usage: { reviews: 2, lastAt: day(18), lastSubject: 'APP-8688' } }),
  rule(22, { revisions: [rev(1, draft('guidance', 'No new dependencies without a ticket note', '', [], []), day(16))],
    usage: { reviews: 4, lastAt: day(20), lastSubject: 'APP-8701' } }),
  rule(30, { origin: 'agent', approval: 'pending', source: { kind: 'proposal', flow: 12, role: 'correctness', finding: null },
    sourceSubject: 'APP-8701', revisions: [rev(1, draft('guidance', 'Prefer select_related for parent lookups in export loops',
      'A query per row blows past the export time limit.', ['exports', 'backend'], ['exports/**/*.py']), day(20))] }),
];
let nextId = 40;
const now = () => day(21);
const refuse = (message, detail) => { throw { kind: 'validation', message, detail }; };
const checked = (d) => {
  for (const glob of d.scope.paths)
    if (glob.startsWith('/') || glob.split('/').includes('..'))
      refuse('\`' + glob + '\`: a path glob cannot climb out of the repository with \`..\`', { kind: 'bad_glob', glob });
  if (d.text.trim() === '') refuse('a rule needs some text', { kind: 'empty_text' });
  // Lower-cased as \`RuleDraft::checked\` does, so the pane shows what the store would keep.
  return { ...structuredClone(d), scope: { ...d.scope, areas: d.scope.areas.map((a) => a.toLowerCase()) } };
};
const find = (id) => rules.find((r) => r.id === id) ?? refuse('no rule ' + id, null);
const create = (extra, d) => { const kept = checked(d); const r = rule(nextId++, { revisions: [rev(1, kept, now())], updatedAt: now(), ...extra }); rules.push(r); return r; };
const change = (r, c) => {
  const append = (d) => r.revisions.push(rev(r.revisions.length + 1, checked(d), now()));
  switch (c.kind) {
    case 'approve': if (r.approval !== 'pending') refuse('not pending', null); r.approval = 'approved'; break;
    case 'reject': if (r.approval !== 'pending') refuse('not pending', null); r.approval = 'rejected'; break;
    case 'approve_revised': if (r.approval !== 'pending') refuse('not pending', null); append(c.draft); r.approval = 'approved'; break;
    case 'revise': append(c.draft); break;
    case 'disable': r.status = 'disabled'; break;
    case 'enable': r.status = 'active'; break;
  }
  r.updatedAt = now();
  return r;
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
    case 'list_rules': return structuredClone(rules);
    case 'instruction_sources': return {
      patterns: ['CLAUDE.md', 'AGENTS.md', 'REVIEW.md', 'docs/review/**'], configured: false,
      documents: [
        { path: 'CLAUDE.md', pattern: 'CLAUDE.md', truncated: false },
        { path: 'REVIEW.md', pattern: 'REVIEW.md', truncated: false },
        { path: 'docs/review/focus.md', pattern: 'docs/review/**', truncated: false },
      ],
      unmatched: ['AGENTS.md'], skipped: [], incomplete: false };
    case 'create_rule': return structuredClone(create({}, a.draft));
    case 'save_finding_as_rule': {
      // The command takes the finding's id and reads the stored finding; this is that store.
      const finding = window.openedFinding;
      if (!finding || finding.id !== a.findingId) refuse('there is no finding ' + a.findingId);
      if (finding.dismissed) refuse('a dismissed finding cannot become a rule', { kind: 'dismissed_finding' });
      return structuredClone(create({ source: { kind: 'finding', flow: finding.flow, finding: finding.id, location: finding.location }, sourceSubject: 'APP-8701' }, a.draft));
    }
    case 'change_rule': return structuredClone(change(find(a.ruleId), a.change));
    case 'replace_rule': {
      const old = find(a.ruleId);
      const created = create({ source: { kind: 'replacement', replaces: old.id } }, a.draft);
      old.status = 'superseded'; old.supersededBy = created.id; old.updatedAt = now();
      return structuredClone(created);
    }
    default: return cmd.startsWith('list_') ? [] : null;
  }
}, { shouldMockEvents: true });
await import('@xterm/xterm/css/xterm.css');
await import('/src/styles/main.scss');
const { mount, unmount } = await import('svelte');
const { default: App } = await import('/src/App.svelte');
const { default: RuleDialog } = await import('/src/lib/components/RuleDialog.svelte');
const { view } = await import('/src/lib/state/view.svelte.ts');
const { workspace } = await import('/src/lib/state/workspace.svelte.ts');
window.harness = {
  emit, view, workspace,
  openFinding(finding) {
    window.openedFinding = finding;
    const target = document.createElement('div');
    document.body.append(target);
    const dialog = mount(RuleDialog, { target, props: {
      projectId: '/x/webapp', projectName: 'webapp', mode: { kind: 'finding', finding, subject: 'APP-8701' },
      onclose: () => unmount(dialog),
      onsaved: (saved) => { window.savedFromFinding = saved; unmount(dialog); },
    } });
  },
};
mount(App, { target: document.getElementById('app') });`;

const server = await createServer({
    configFile: false,
    envDir: false,
    logLevel: 'warn',
    plugins: [
        svelte(),
        {
            name: 'knowledge-probe',
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

const FINDING = {
    id: 31,
    flow: 12,
    class: { kind: 'suggestion' },
    severity: 'notable',
    location: { path: 'exports/export.py', startLine: 88, endLine: 88 },
    title: 'Load parent records with select_related when exporting units.',
    rationale:
        'Exports run over thousands of units, and a query per row blows past the export time limit.',
    requirement: null,
    treeSha: '9f00',
    dismissed: null,
};

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

    /** A fresh window at its final size: a capture after `setViewportSize` shows the old frame. */
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
    const filter = (page, name) =>
        settings(page)
            .getByRole('group', { name: 'Filter rules' })
            .getByRole('button', {
                name: new RegExp(`^${name}`),
            });
    const rows = (page) => settings(page).locator('.c-rule-row');
    const detail = (page) =>
        settings(page).getByRole('region', { name: 'Rule', exact: true });

    async function openKnowledge(page) {
        await page.getByRole('button', { name: 'Settings', exact: true }).click();
        await settings(page).getByRole('tab', { name: 'Knowledge' }).click();
        await settings(page).getByText('Review knowledge', { exact: true }).waitFor();
        await rows(page).first().waitFor();
        await settings(page).getByText('Found', { exact: false }).first().waitFor();
    }

    // ── Off: no Knowledge section ─────────────────────────────────────────────────────────────
    {
        const page = await open('?work=off');
        await page.getByRole('button', { name: 'Settings', exact: true }).click();
        await settings(page).getByRole('tab', { name: 'General' }).waitFor();
        assert.equal(
            await settings(page).getByRole('tab', { name: 'Knowledge' }).count(),
            0,
            'Knowledge belongs to the Work view and is hidden while it is off',
        );
        await page.context().close();
    }

    // ── On: the design's state, light and dark ────────────────────────────────────────────────
    for (const scheme of ['light', 'dark']) {
        const page = await open(`?work=on&theme=${scheme}`, scheme);
        await openKnowledge(page);
        assert.deepEqual(
            await settings(page)
                .getByRole('group', { name: 'Filter rules' })
                .getByRole('button')
                .allTextContents(),
            ['Active 4', 'Proposed 1', 'Disabled 1', 'Superseded 2'],
        );
        assert.equal(await filter(page, 'Active').getAttribute('aria-pressed'), 'true');
        assert.deepEqual(await rows(page).locator('.c-rule-row__text').allTextContents(), [
            "Validate dates in the record's own timezone",
            'Use AppDialog for confirm and cancel flows',
            'Export file names use kebab-case',
            'No new dependencies without a ticket note',
        ]);
        assert(
            (await settings(page).locator('.c-proposal').textContent()).includes(
                'Prefer select_related for parent lookups in export loops',
            ),
            'a pending proposal is shown beneath the active list',
        );
        await rows(page).nth(1).click();
        const text = await detail(page).textContent();
        for (const expected of [
            'R-14 · version 2 · active · yours',
            'Required · breaking it is reported as a rule finding',
            'Repository webapp · area frontend · src/**/*.vue',
            'src/components/AppDialog.vue',
            'src/components/BaseModal.vue for confirmations',
            'You, from a finding on APP-8642 · Oct 12 · active on save',
            '6 reviews · last on APP-8701, Oct 20',
            'v2 · Oct 15 · you',
            'v1 · Oct 12 · you',
            'Reviews also read CLAUDE.md, AGENTS.md, REVIEW.md and docs/review/**.',
            'Found CLAUDE.md, REVIEW.md, docs/review/focus.md.',
            'Nothing matches AGENTS.md.',
        ])
            assert(text.includes(expected), `detail should say "${expected}": ${text}`);
        assert(
            await detail(page)
                .getByRole('button', {
                    name: 'Propose this rule to the repository · later',
                })
                .isDisabled(),
        );
        await shot(page, join(shots, `knowledge-${scheme}.png`));
        await page.context().close();
    }

    // ── Changes, each sending its command and landing on the rule it changed ─────────────────
    {
        const page = await open('?work=on');
        await openKnowledge(page);

        // Approve the proposal from its card.
        await settings(page)
            .locator('.c-proposal')
            .getByRole('button', { name: 'Approve', exact: true })
            .click();
        await settings(page).locator('.c-proposal').waitFor({ state: 'detached' });
        assert.deepEqual(await calls(page, 'change_rule'), [
            { projectId: '/x/webapp', ruleId: 30, change: { kind: 'approve' } },
        ]);
        assert.equal(await filter(page, 'Active').textContent(), 'Active 5');
        assert(
            (await detail(page).textContent()).includes(
                'R-30 · version 1 · active · an agent',
            ),
        );

        // New rule, with a glob the backend refuses first.
        await settings(page).getByRole('button', { name: 'New rule' }).click();
        const dialog = page.getByRole('dialog', { name: 'New review rule' });
        await dialog
            .getByLabel('Rule', { exact: true })
            .fill('Confirm destructive actions with AppDialog');
        await dialog
            .getByLabel('Why', { exact: true })
            .fill('A typed confirmation protects bulk deletes.');
        await dialog.getByRole('radio', { name: /Required/ }).check();
        await dialog.getByLabel('Areas', { exact: true }).fill('Frontend, ui');
        await dialog.getByLabel('Files', { exact: true }).fill('../elsewhere/**');
        await dialog.getByRole('button', { name: 'Save rule' }).click();
        await dialog.getByText('cannot climb out of the repository').waitFor();
        assert(await dialog.isVisible(), 'a refused glob leaves the dialog open');
        await shot(page, join(shots, 'new-rule-refused-light.png'));
        await dialog.getByLabel('Files', { exact: true }).fill('src/**/*.vue, src/**/*.ts');
        await dialog
            .getByLabel('Preferred example', { exact: true })
            .fill('src/components/AppDialog.vue');
        await dialog.getByRole('button', { name: 'Save rule' }).click();
        await dialog.waitFor({ state: 'detached' });
        const created = (await calls(page, 'create_rule')).at(-1);
        assert.deepEqual(created.draft, {
            kind: 'required',
            text: 'Confirm destructive actions with AppDialog',
            rationale: 'A typed confirmation protects bulk deletes.',
            scope: { areas: ['Frontend', 'ui'], paths: ['src/**/*.vue', 'src/**/*.ts'] },
            examples: [{ stance: 'prefer', text: 'src/components/AppDialog.vue' }],
            references: [],
        });
        {
            const shown = await detail(page).textContent();
            assert(shown.includes('R-40 · version 1 · active · yours'), shown);
        }

        // Edit R-14: a third version, with the second kept in its history.
        await rows(page).filter({ hasText: 'Use AppDialog for confirm' }).click();
        await detail(page).getByRole('button', { name: 'Edit', exact: true }).click();
        const edit = page.getByRole('dialog', { name: 'Edit R-14' });
        assert.equal(
            await edit.getByLabel('Rule', { exact: true }).inputValue(),
            'Use AppDialog for confirm and cancel flows',
        );
        await edit
            .getByLabel('Rule', { exact: true })
            .fill('Use AppDialog for every confirmation');
        await edit.getByRole('button', { name: 'Save version 3' }).click();
        await edit.waitFor({ state: 'detached' });
        assert.equal((await calls(page, 'change_rule')).at(-1).change.kind, 'revise');
        const history = await detail(page).locator('.c-revision__meta').allTextContents();
        assert.deepEqual(history, [
            'v3 · Oct 21 · you',
            'v2 · Oct 15 · you',
            'v1 · Oct 12 · you',
        ]);

        // Off, then on again, following the rule between filters.
        await detail(page).getByRole('button', { name: 'Disable', exact: true }).click();
        await filter(page, 'Disabled 2').waitFor();
        assert.equal(await filter(page, 'Disabled').getAttribute('aria-pressed'), 'true');
        assert((await detail(page).textContent()).includes('R-14 · version 3 · disabled'));
        await detail(page).getByRole('button', { name: 'Turn on', exact: true }).click();
        await filter(page, 'Disabled 1').waitFor();
        assert.equal(await filter(page, 'Active').getAttribute('aria-pressed'), 'true');

        // Replace R-22: the old one is superseded and points at the new one.
        await rows(page).filter({ hasText: 'No new dependencies' }).click();
        await detail(page).getByRole('button', { name: 'Replace…' }).click();
        const replace = page.getByRole('dialog', { name: 'Replace R-22' });
        await replace
            .getByLabel('Rule', { exact: true })
            .fill('No new dependencies without a ticket and a licence check');
        await replace.getByRole('button', { name: 'Replace rule' }).click();
        await replace.waitFor({ state: 'detached' });
        assert.deepEqual((await calls(page, 'replace_rule')).at(-1).ruleId, 22);
        assert((await detail(page).textContent()).includes('You, replacing R-22'));
        await filter(page, 'Superseded').click();
        await rows(page)
            .filter({ hasText: 'No new dependencies without a ticket note' })
            .click();
        await detail(page).getByRole('button', { name: 'Show R-41' }).click();
        assert((await detail(page).textContent()).includes('R-41 · version 1 · active'));

        // Turning Work off with Knowledge open falls back to General.
        await page.evaluate(() => window.harness.view.setWorkEnabled(false));
        await settings(page).getByRole('tab', { name: 'General' }).waitFor();
        assert.equal(
            await settings(page)
                .getByRole('tab', { name: 'General' })
                .getAttribute('aria-selected'),
            'true',
        );
        assert.equal(
            await settings(page).getByRole('tab', { name: 'Knowledge' }).count(),
            0,
        );
        await page.context().close();
    }

    // ── Edit first: a proposal approved as you edited it ─────────────────────────────────────
    {
        const page = await open('?work=on');
        await openKnowledge(page);
        await filter(page, 'Proposed').click();
        await settings(page)
            .locator('.c-proposal')
            .getByRole('button', { name: 'Edit first' })
            .click();
        const review = page.getByRole('dialog', { name: 'Edit and approve R-30' });
        await review.getByText('Proposed by the correctness role on APP-8701').waitFor();
        await review
            .getByLabel('Rule', { exact: true })
            .fill('Prefer select_related for parent lookups');
        await review.getByRole('button', { name: 'Save and approve' }).click();
        await review.waitFor({ state: 'detached' });
        const sent = (await calls(page, 'change_rule')).at(-1);
        assert.equal(sent.change.kind, 'approve_revised');
        assert.equal(sent.change.draft.text, 'Prefer select_related for parent lookups');
        assert.equal(await filter(page, 'Proposed').textContent(), 'Proposed 0');
        await page.context().close();
    }

    // ── Save as rule, from a finding, light and dark ─────────────────────────────────────────
    for (const scheme of ['light', 'dark']) {
        const page = await open(`?work=on&theme=${scheme}`, scheme, 1440, 920);
        await page.evaluate((finding) => window.harness.openFinding(finding), FINDING);
        const dialog = page.getByRole('dialog', { name: 'Save as review rule' });
        await dialog.waitFor();
        assert.equal(
            (await dialog.locator('.c-rule-form__from').textContent())
                .replace(/\s+/g, ' ')
                .trim(),
            'From finding Notable exports/export.py:88 on APP-8701',
        );
        assert.equal(
            await dialog.getByLabel('Rule', { exact: true }).inputValue(),
            FINDING.title,
        );
        assert.equal(
            await dialog.getByLabel('Why', { exact: true }).inputValue(),
            FINDING.rationale,
        );
        assert.equal(
            await dialog.getByLabel('Files', { exact: true }).inputValue(),
            'exports/export.py',
        );
        assert(await dialog.getByRole('radio', { name: /Guidance/ }).isChecked());
        assert(
            await dialog
                .getByRole('checkbox', { name: /Check it against the repository/ })
                .isDisabled(),
            'the conflict check is not built yet and says so',
        );
        assert(
            (await dialog.textContent()).includes(
                'You wrote this rule, so it is active as soon as it',
            ),
        );
        await dialog.getByLabel('Areas', { exact: true }).fill('exports, backend');
        await dialog.getByLabel('Files', { exact: true }).fill('exports/**/*.py');
        await dialog
            .getByLabel('Preferred example', { exact: true })
            .fill('exports/sites.py:30');
        await shot(page, join(shots, `save-as-rule-${scheme}.png`));
        if (scheme === 'light') {
            await dialog.getByRole('button', { name: 'Save rule' }).click();
            await dialog.waitFor({ state: 'detached' });
            const [sent] = await calls(page, 'save_finding_as_rule');
            assert.equal(
                sent.findingId,
                31,
                'the dialog sends the stored finding by its id',
            );
            assert.deepEqual(sent.draft.scope, {
                areas: ['exports', 'backend'],
                paths: ['exports/**/*.py'],
            });
            assert.equal(
                await page.evaluate(() => window.savedFromFinding.source.kind),
                'finding',
            );
        }
        await page.context().close();
    }

    // ── Narrow and dark: the columns stack ───────────────────────────────────────────────────
    {
        const page = await open('?work=on&theme=dark', 'dark', 860, 760);
        await openKnowledge(page);
        const list = await settings(page).locator('.c-knowledge__rules').boundingBox();
        const pane = await detail(page).boundingBox();
        assert(
            pane.y >= list.y + list.height - 1,
            'narrow, the detail sits below the list',
        );
        await shot(page, join(shots, 'knowledge-narrow-dark.png'));
        await page.context().close();
    }

    console.log(`ok — screenshots in ${shots}`);
} finally {
    await browser?.close();
    await server.close();
}

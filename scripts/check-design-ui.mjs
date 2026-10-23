/**
 * Drive the real app shell with mocked IPC: an issue's Design tab, from a revision ready for your
 * decision to implementing. No Tauri, no CLIs, no `acli`.
 *
 * What it proves, in order: the design-ready inbox item leads to the Design tab rather than
 * offering to pause the run; the tab says implementing waits, and Start implementing, clicked
 * anyway, shows Rust's refusal and moves nothing; a comment is kept with its revision; Approve
 * sends the revision and the fingerprint the tab showed, and the gate opens; Request changes sends
 * your note, and the next revision shows the approval of the first as replaced while the first
 * stays on the page, still previewable; Preview opens the review pane in the issue's worktree;
 * approving the newest revision lets Start implementing through. Then two states the live
 * click-through did not reach: an approval made stale by a changed source file, and a plan that
 * needs a design in a project that names no designer.
 *
 * The mocked backend keeps what Rust would: the revisions, comments and decisions, the standing,
 * and the gate's answer, which Start implementing asks again.
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
const shots = process.env.WTM_SHOTS || join(tmpdir(), 'wtm-design');
mkdirSync(shots, { recursive: true });

const entry = `
import { emit } from '@tauri-apps/api/event';
import { mockIPC, mockWindows } from '@tauri-apps/api/mocks';
const params = new URLSearchParams(location.search);
const scenario = params.get('scenario') ?? 'review';
const prefKey = (k) => 'harness.pref.' + k;
localStorage.setItem(prefKey('ui.notify'), 'off');
localStorage.setItem(prefKey('ui.work_view'), 'on');
localStorage.setItem('wtm.view', 'work');
document.documentElement.dataset.theme = params.get('theme') ?? 'light';
document.documentElement.dataset.palette = 'pine';
document.documentElement.dataset.platform = 'macos';
mockWindows('main');
const KEY = 'ACME-301';
const FLOW = 40;
const WT = '/x/' + KEY;
const project = { id: '/x/webapp', name: 'webapp', root: '/x/webapp', usable: true, problem: null, trust: null };
const wt = (dir, branch) => ({
  id: dir === 'webapp' ? '/x/webapp' : '/x/' + dir, title: dir, subtitle: branch ?? '(detached)',
  path: dir === 'webapp' ? '/x/webapp' : '/x/' + dir, dirname: dir, branch, head: 'a1b2c3d4e5',
  isMain: dir === 'webapp', isBare: false, locked: null, prunable: null, dirty: false, untracked: 0,
  staged: 0, ahead: 0, behind: 0, issueKey: null, badges: [], links: [], table: [], env: [],
  browserHome: null });
const worktrees = [wt('webapp', 'main'), wt(KEY, 'feature/' + KEY)];
const status = { name: 'In Progress', category: 'in_progress' };
const issue = { key: KEY, summary: 'A Units screen for one building', status, kind: 'Story', priority: 'High', assignee: null, labels: [] };
const sprint = { id: 41, name: 'Sprint 41', state: 'active', goal: null, startsAt: null, endsAt: null, board: 3 };
const columns = [{ role: 'todo', status: 'To Do' }, { role: 'started', status: 'In Progress' }, { role: 'done', status: 'Done' }];

/* Rust's record, kept as the engine keeps it. */
const state = scenario === 'nodesigner'
  ? { phase: 'designing', hold: null }
  : { phase: 'design_ready', hold: scenario === 'stale' ? null : { kind: 'awaiting_approval' } };
const sources = [{ path: 'src/components/PageHeader.svelte', fingerprint: 'f1' },
  { path: 'src/styles/tokens.css', fingerprint: 'f2' }];
const revision = (n, feedback, reuse) => ({ id: n, flow: FLOW, revision: n, prototype: 'p1', review: 'r' + n,
  fingerprint: n === 1 ? 'afc476fe5e0c54e4' : 'c01651b3a81f5ebe',
  brief: n === 1 ? 'A Units screen: PageHeader, a DataTable of units, a Vacant only filter' : 'Revise the filter: a quiet Button with the vacant count',
  feedback, reuse, sources, planRevision: 1, step: 5, invocation: 50 + n, submittedAt: Date.now() });
const revisions = scenario === 'nodesigner' ? [] : [revision(1, null,
  'Header: **PageHeader** from src/components/PageHeader.svelte. Table: **DataTable**. Filter: a primary Button, approximated.')];
const comments = [];
const decisions = scenario === 'stale' ? [{ id: 1, flow: FLOW, revision: 1, verdict: { kind: 'approved', approval: 91 }, at: Date.now() }] : [];
let standing = scenario === 'stale'
  ? { kind: 'stale', approval: 91, revision: 1, changes: [{ pin: 'sources', approved: 'sha256:a', current: 'sha256:b' }] }
  : scenario === 'nodesigner' ? { kind: 'none' } : { kind: 'waiting', revision: 1, replaced: null };
let running = false;
const blocked = () => {
  switch (standing.kind) {
    case 'approved': return null;
    case 'none': return { kind: 'no_design' };
    case 'waiting': return { kind: 'design_not_approved', revision: standing.revision };
    case 'revising': return { kind: 'design_revising', revision: standing.revision };
    case 'stale': return { kind: 'design_stale', revision: standing.revision };
  }
};
const REASON = { no_design: 'the plan needs a design, and no design revision has been published yet',
  design_not_approved: 'the plan needs a design, and design revision {n} waits for your approval',
  design_revising: 'you asked for changes to design revision {n}, and the designer is revising it',
  design_stale: 'your approval of design revision {n} no longer covers it' };
const reason = (b) => b ? REASON[b.kind].replace('{n}', b.revision) : null;
const flowView = () => ({ id: FLOW, workItem: FLOW, state: { kind: 'implement', phase: state.phase, hold: state.hold, end: null },
  seq: 9, startedAt: Date.now(), updatedAt: Date.now() });
const session = (attempt) => ({ id: 50 + attempt, attempt, provider: 'claude', model: 'sonnet', session: 's' + attempt,
  providerSession: '254d9dd8-c798-4dd4-af11-a399db6d3896', status: 'submitted', detail: null, progress: null,
  startedAt: Date.now(), endedAt: Date.now() });
const designView = () => {
  const b = blocked();
  const open = !running;
  return { flow: flowView(), key: KEY, designer: scenario === 'nodesigner' ? null : 'designer',
    design: scenario === 'nodesigner' ? null : { flow: FLOW, project: '/x/webapp', worktree: WT, key: KEY, role: 'designer',
      origin: 'plan', request: null, prototype: 'p1', createdAt: Date.now(), updatedAt: Date.now() },
    revisions: revisions.map((r) => ({ revision: structuredClone(r), readable: true,
      changedSources: scenario === 'stale' && r.revision === 1 ? ['src/components/PageHeader.svelte'] : [],
      artifact: '/data/wtm/prototypes/p1/reviews/' + r.review })),
    comments: structuredClone(comments), decisions: structuredClone(decisions), standing: structuredClone(standing),
    planRevision: 1, designNeeded: true, skipDesign: false, required: true,
    gate: { ready: b === null, blocked: b, reason: reason(b) },
    sessions: revisions.map((r) => session(r.revision)), step: null, running,
    canRequest: false, canStart: false,
    canDecide: open && state.phase === 'design_ready' && revisions.length > 0,
    canImplement: open && (state.phase === 'planned' || state.phase === 'design_ready') };
};
const inbox = scenario === 'review' ? [{ id: 1, flow: FLOW, kind: 'approval', target: { kind: 'flow' },
  title: KEY + ': design revision 1 is ready',
  body: 'Preview it in the issue\\'s Design tab, comment on it, and approve it or ask for changes.',
  state: 'open', createdAt: Date.now(), closedAt: null, project: '/x/webapp', worktree: WT, issue: KEY, question: null }] : [];
const changed = () => setTimeout(() => emit('flow:changed', { flow: FLOW }), 0);
window.publish2 = () => {
  revisions.push(revision(2, 'Make the filter read as a filter.\\n\\nComments on revision 1:\\n- Filter: use a quiet Button with the vacant count',
    'Filter: a quiet **Button** showing "Vacant only (3)". Everything else as revision 1.'));
  running = false;
  state.phase = 'design_ready';
  state.hold = { kind: 'awaiting_approval' };
  standing = { kind: 'waiting', revision: 2, replaced: { approval: 91, revision: 1 } };
  changed();
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
    case 'tracker_queue':
    case 'sync_tracker': {
      const queue = { tracker: 'jira', query: 'assignee = currentUser()', board: 3, columns, sprints: [sprint],
        issues: [{ tracker: 'jira', issue: structuredClone(issue), sprint: 41, column: 1, role: 'started', workItem: FLOW,
          worktrees: [WT], run: { flow: FLOW, worktree: WT, state: flowView().state,
            openItems: inbox.length, start: 'ready' }, section: inbox.length > 0 ? 'needs_you' : 'running' }],
        readAt: Date.now(), failure: null, outdated: false };
      return cmd === 'sync_tracker' ? { queue, report: null } : queue;
    }
    case 'tracker_issue':
      return { tracker: 'jira', readAt: Date.now(), column: 1, role: 'started', account: null,
        detail: { issue: structuredClone(issue), description: 'A new screen; product approves a design first.',
          parent: null, updated: null, sprints: [sprint], links: [], fields: [] } };
    case 'issue_start':
      return { workItem: FLOW, flow: flowView(), job: { workItem: FLOW, flow: FLOW, project: '/x/webapp', key: KEY,
        candidate: { kind: 'new', branch: 'feature/' + KEY, directory: WT, base: 'main' }, worktree: WT,
        branch: 'feature/' + KEY, state: 'ready', detail: null, setupSession: null, createdAt: Date.now(), updatedAt: Date.now() },
        working: false, moves: [], inbox: structuredClone(inbox), startMove: { kind: 'stay', reason: 'already_there' },
        startStatus: 'In Progress' };
    case 'issue_plan': return null;
    case 'issue_design': return designView();
    case 'comment_on_design': {
      const c = { id: comments.length + 1, flow: FLOW, revision: a.revision, text: a.text, at: Date.now() };
      comments.push(c);
      changed();
      return c;
    }
    case 'approve_design':
      decisions.push({ id: decisions.length + 1, flow: FLOW, revision: a.revision, verdict: { kind: 'approved', approval: 90 + a.revision }, at: Date.now() });
      standing = { kind: 'approved', approval: 90 + a.revision, revision: a.revision };
      state.hold = null;
      inbox.splice(0, inbox.length);
      changed();
      return decisions.at(-1);
    case 'request_design_changes':
      decisions.push({ id: decisions.length + 1, flow: FLOW, revision: a.revision, verdict: { kind: 'changes_requested', note: a.note }, at: Date.now() });
      standing = { kind: 'revising', revision: a.revision, note: a.note };
      state.phase = 'designing';
      state.hold = null;
      running = true;
      changed();
      return decisions.at(-1);
    case 'preview_design_revision':
      return { id: 'browser-preview', project: '/x/webapp', worktree: WT, prototypeId: 'p1/r' + a.revision,
        url: 'about:blank', title: 'Prototype review', loading: false, loadError: null, canGoBack: false,
        canGoForward: false, agentAccess: false, openedBy: null, commentMode: false, agentDriving: null };
    case 'start_implementing': {
      const b = blocked();
      if (b) throw { kind: 'flow', message: 'Not yet: ' + reason(b) + '.' };
      state.phase = 'implementing';
      changed();
      return null;
    }
    case 'work_inbox': return structuredClone(inbox);
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
            name: 'design-probe',
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

    async function open(query, colorScheme) {
        const context = await browser.newContext({
            viewport: { width: 1280, height: 900 },
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
        await queue(page).getByText('ACME-301').waitFor();
        return page;
    }

    const shot = (page, path) => page.screenshot({ path, animations: 'disabled' });
    const calls = (page, cmd) =>
        page.evaluate((c) => window.calls.filter((x) => x.cmd === c), cmd);
    const queue = (page) => page.getByRole('navigation', { name: 'Work queue' });
    const row = (page, key) => queue(page).locator('.c-work-row').filter({ hasText: key });
    const panel = (page) => page.getByRole('tabpanel');
    const tab = (page) => panel(page).locator('.c-design');
    const card = (page, n) =>
        tab(page).locator('.c-design-revision').filter({ hasText: `Design · revision ${n}` });
    const gate = (page) => tab(page).locator('.c-design__gate');

    for (const scheme of ['light', 'dark']) {
        const page = await open(`?theme=${scheme}`, scheme);

        // ── The design-ready item leads to the Design tab; it is not an offer to pause ───────
        await page
            .getByRole('button', { name: /^Inbox/ })
            .first()
            .click();
        const ready = page
            .locator('.c-inbox-card')
            .filter({ hasText: 'design revision 1 is ready' });
        await ready.waitFor();
        assert.equal(await ready.getByRole('button', { name: 'Pause the run' }).count(), 0);
        assert.equal(await ready.getByRole('button', { name: 'Read the plan' }).count(), 0);
        await shot(page, join(shots, `inbox-design-ready-${scheme}.png`));
        await ready.getByRole('button', { name: 'Review the design' }).click();

        // ── The gate says implementing waits; clicking anyway is refused and moves nothing ──
        await card(page, 1).waitFor();
        await gate(page)
            .getByText('Implementing waits: the design waits for your approval.')
            .waitFor();
        const shown = await card(page, 1).innerText();
        for (const part of [
            'waits for you',
            'afc476fe5e',
            'for plan revision 1',
            'PageHeader',
            'src/styles/tokens.css',
        ])
            assert(
                shown.toLowerCase().includes(part.toLowerCase()),
                `revision 1 shows “${part}”:\n${shown}`,
            );
        await tab(page).getByRole('button', { name: 'Start implementing' }).click();
        await tab(page)
            .getByText('Not yet: the plan needs a design, and design revision 1 waits for your approval.')
            .waitFor();
        assert.equal((await calls(page, 'start_implementing')).length, 1);
        await shot(page, join(shots, `design-refused-${scheme}.png`));

        // ── A comment is kept with its revision ─────────────────────────────────────────────
        await card(page, 1)
            .getByRole('textbox', { name: 'Comment on revision 1' })
            .fill('Filter: use a quiet Button with the vacant count');
        await card(page, 1).getByRole('button', { name: 'Comment', exact: true }).click();
        await card(page, 1)
            .locator('.c-design-revision__comment')
            .getByText('Filter: use a quiet Button with the vacant count')
            .waitFor();
        assert.deepEqual((await calls(page, 'comment_on_design'))[0].args, {
            flow: 40,
            revision: 1,
            text: 'Filter: use a quiet Button with the vacant count',
        });

        // ── Approve sends what the tab showed, and the gate opens ───────────────────────────
        await card(page, 1).getByRole('button', { name: 'Approve revision 1' }).click();
        await gate(page).getByText(/Implementing may start/).waitFor();
        assert.deepEqual((await calls(page, 'approve_design'))[0].args, {
            flow: 40,
            revision: 1,
            fingerprint: 'afc476fe5e0c54e4',
        });
        await shot(page, join(shots, `design-approved-${scheme}.png`));

        // ── Request changes: the note goes, and the next revision replaces the approval ─────
        await card(page, 1).getByRole('button', { name: 'Request changes' }).click();
        await card(page, 1)
            .getByRole('textbox', { name: 'What should change?' })
            .fill('Make the filter read as a filter.');
        await card(page, 1).getByText(/and the 1 comment on this revision/).waitFor();
        await card(page, 1).getByRole('button', { name: 'Send to the designer' }).click();
        await tab(page)
            .locator('.c-design__note')
            .filter({ hasText: /The designer is drawing\s+revision 2/ })
            .waitFor();
        assert.deepEqual((await calls(page, 'request_design_changes'))[0].args, {
            flow: 40,
            revision: 1,
            note: 'Make the filter read as a filter.',
        });
        await page.evaluate(() => window.publish2());
        await card(page, 2).getByText('waits for you').waitFor();
        await card(page, 1).getByText('approval replaced').waitFor();
        await tab(page)
            .getByText(/It replaced revision 1, so your approval of that one no longer stands/)
            .waitFor();
        await gate(page)
            .getByText('Implementing waits: the design waits for your approval.')
            .waitFor();
        assert.match(await card(page, 2).innerText(), /Comments on revision 1/);
        await shot(page, join(shots, `design-replaced-${scheme}.png`));

        // ── The earlier revision is still there to preview, in the issue's worktree ─────────
        await card(page, 1).getByRole('button', { name: 'Preview' }).click();
        await page.waitForFunction(() => window.harness.view.current === 'worktree');
        assert.deepEqual((await calls(page, 'preview_design_revision'))[0].args, {
            flow: 40,
            revision: 1,
        });
        await page.evaluate(() => window.harness.view.show('work'));
        await row(page, 'ACME-301').click();
        await page.getByRole('tab', { name: 'Design' }).click();
        await panel(page).locator('.c-design').waitFor();

        // ── Approving the newest lets implementing through ──────────────────────────────────
        await card(page, 2).getByRole('button', { name: 'Approve revision 2' }).click();
        await gate(page).getByText(/and so is design revision 2/).waitFor();
        await tab(page).getByRole('button', { name: 'Start implementing' }).click();
        await gate(page).getByText(/^Implementing\. The plan and the design/).waitFor();
        await shot(page, join(shots, `design-implementing-${scheme}.png`));
        await page.context().close();

        // ── A source file changed after approval: stale, with the file named ────────────────
        const stale = await open(`?theme=${scheme}&scenario=stale`, scheme);
        await row(stale, 'ACME-301').click();
        await stale.getByRole('tab', { name: 'Design' }).click();
        await card(stale, 1).getByText('approval stale').waitFor();
        await card(stale, 1)
            .getByText(/Changed since it was drawn:/)
            .waitFor();
        await gate(stale)
            .getByText('Implementing waits: the design approval is stale.')
            .waitFor();
        await tab(stale)
            .getByText(/no longer covers what it approved: a project file it was drawn from changed/)
            .waitFor();
        assert.equal(
            await card(stale, 1).getByRole('button', { name: 'Approve revision 1' }).isDisabled(),
            true,
            'a revision whose sources changed is not offered for approval',
        );
        await shot(stale, join(shots, `design-stale-${scheme}.png`));
        await stale.context().close();

        // ── A plan that needs a design, in a project with no designer ───────────────────────
        const none = await open(`?theme=${scheme}&scenario=nodesigner`, scheme);
        await row(none, 'ACME-301').click();
        await none.getByRole('tab', { name: 'Design' }).click();
        await tab(none).getByText(/This project names no designer/).waitFor();
        await gate(none)
            .getByText('Implementing waits: the plan needs a design, and there is none yet.')
            .waitFor();
        assert.equal(
            await tab(none).getByRole('button', { name: /designer/ }).count(),
            0,
            'nothing to start without a designer',
        );
        await shot(none, join(shots, `design-no-designer-${scheme}.png`));
        await none.context().close();
    }

    console.log(`ok — screenshots in ${shots}`);
} finally {
    await browser?.close();
    await server.close();
}

/**
 * Drive the real app shell with mocked IPC: an issue's planner, its question, Discuss, the answer,
 * Send to Jira's two clicks, the plan decision and a stale approval. No Tauri, no CLIs, no `acli`.
 *
 * What it proves, in order: the Run section says the run waits for your answer and leads to the
 * Plan tab; the Plan tab shows the planner's question as the inbox does, with Why, Unblocks,
 * Context and the Recommendation, and Send to Jira… off until it is answered; Discuss sends your
 * message and shows the planner's reply in the thread, the question still open; the same question
 * in the inbox carries the same thread, and answering there resumes the run; the plan-ready item
 * in the inbox reads the plan rather than offering to pause the run; the plan is shown whole with
 * Approve, Approve and skip design and Request changes; Send to Jira… opens a draft and posts
 * nothing until Post, which sends exactly the edited text; Approve sends the issue's revision the
 * tab showed; and an edit to the issue makes the approval stale with a line diff.
 *
 * The mocked backend keeps what Rust would: one question, answered once; a draft that records
 * nothing; a comment posted only by `post_question_comment`; and the plan's standing.
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
const shots = process.env.WTM_SHOTS || join(tmpdir(), 'wtm-plan');
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
const KEY = 'ACME-201';
const FLOW = 30;
const project = { id: '/x/webapp', name: 'webapp', root: '/x/webapp', usable: true, problem: null, trust: null };
const wt = (dir, branch) => ({
  id: dir === 'webapp' ? '/x/webapp' : '/x/' + dir, title: dir, subtitle: branch ?? '(detached)',
  path: dir === 'webapp' ? '/x/webapp' : '/x/' + dir, dirname: dir, branch, head: 'a1b2c3d4e5',
  isMain: dir === 'webapp', isBare: false, locked: null, prunable: null, dirty: false, untracked: 0,
  staged: 0, ahead: 0, behind: 0, issueKey: null, badges: [], links: [], table: [], env: [],
  browserHome: null });
const worktrees = [wt('webapp', 'main'), wt(KEY, 'feature/' + KEY)];
const status = { name: 'In Progress', category: 'in_progress' };
const issue = { key: KEY, summary: 'Export the unit list', status, kind: 'Story', priority: 'Medium', assignee: null, labels: [] };
const sprint = { id: 41, name: 'Sprint 41', state: 'active', goal: 'Ship the bulk export', startsAt: null, endsAt: null, board: 3 };
const columns = [{ role: 'todo', status: 'To Do' }, { role: 'started', status: 'In Progress' }, { role: 'done', status: 'Done' }];
const text = (description) => ({ key: KEY, summary: 'Export the unit list', kind: 'Story', description, fields: [], parent: null, links: [] });
const asked = text('Users export the unit list.\\nAs a spreadsheet.');
const edited = text('Users export the unit list.\\nAs XLSX, with a summary sheet.');

/* Rust's record, kept as the engine keeps it. */
const state = { phase: 'investigating', hold: { kind: 'awaiting_answer' } };
const question = { id: 7, flow: FLOW, step: 3, role: 'planner', text: 'CSV or XLSX for the export?',
  why: 'The issue says "spreadsheet", and XLSX needs a new dependency.', unblocks: 'the plan',
  blocks: 'planned', context: [{ kind: 'file', path: 'src/export.py', line: 4, note: null },
    { kind: 'mention', text: 'ACME-201 description: As a spreadsheet.' }],
  recommendation: 'CSV: it reuses the existing writer.', options: ['CSV', 'XLSX'], askedAt: Date.now() - 60000, answer: null };
const discussion = [];
const comments = [];
const plans = [];
const decisions = [];
let standing = { kind: 'none' };
let issueNow = asked;
let replying = false;
const session = (attempt, status, detail) => ({ id: 40 + attempt, attempt, provider: 'claude', model: 'sonnet',
  session: 's' + attempt, providerSession: '3680fa16-7a89-4a1a-88c7-86e112238347', status, detail, progress: null,
  startedAt: Date.now(), endedAt: Date.now() });
const sessions = [session(1, 'interrupted', 'wtm ended the turn after question 7, and closed the session until you answer')];
const thread = () => ({ question: structuredClone(question), discussion: structuredClone(discussion),
  comments: structuredClone(comments), canDiscuss: question.answer === null && !replying, replying, issue: KEY });
const plan = { summary: 'Export the unit list as CSV, in the list\\'s column order.',
  steps: ['Add an Export button to the unit list', 'Write rows with the existing CSV writer'],
  acceptanceCriteria: ['A CSV downloads from the unit list', 'Columns follow the list'],
  verification: ['A test that reads the file back', 'Click through the download'],
  areas: ['src/export.py', 'src/views/UnitList.svelte'], designNeeded: true,
  designReason: 'A new button in the list header.', risks: [] };
const flowView = () => ({ id: FLOW, workItem: FLOW, state: { kind: 'implement', phase: state.phase, hold: state.hold, end: null },
  seq: 4, startedAt: Date.now(), updatedAt: Date.now() });
const planView = () => ({ flow: flowView(), planner: 'planner',
  planning: { flow: FLOW, project: '/x/webapp', worktree: '/x/' + KEY, key: KEY, tracker: 'jira', role: 'planner',
    context: { issue: asked, status: 'In Progress', sprint: 'Sprint 41', sprintGoal: 'Ship the bulk export',
      dependencies: [{ relation: 'is blocked by', key: 'ACME-150', summary: 'Unit model', status: 'Done' }],
      instructionSources: ['CLAUDE.md'], rules: [], skills: [], role: 'planner', provider: 'claude', model: 'sonnet',
      readAt: Date.now() }, createdAt: Date.now(), updatedAt: Date.now() },
  plans: structuredClone(plans), decisions: structuredClone(decisions), standing: structuredClone(standing),
  issueNow, issueRevisionNow: issueNow === asked ? 'sha256:asked' : 'sha256:edited', issueChanges: [],
  questions: [thread()], sessions: structuredClone(sessions), step: null, running: replying, canStart: false });
const inbox = [{ id: 1, flow: FLOW, kind: 'question', target: { kind: 'question', question: 7 },
  title: KEY + ': CSV or XLSX for the export?', body: '', state: 'open', createdAt: Date.now(), closedAt: null,
  project: '/x/webapp', worktree: '/x/' + KEY, issue: KEY, question }];
const changed = () => setTimeout(() => emit('flow:changed', { flow: FLOW }), 0);
window.reply = () => {
  discussion.push({ id: 2, question: 7, from: 'agent', text: 'Two reports import the CSV writer’s column helpers; CSV keeps them untouched.', at: Date.now() });
  replying = false;
  sessions.push(session(2, 'interrupted', 'a discussion turn: wtm closed the session after its reply'));
  changed();
};
window.editIssue = () => {
  issueNow = edited;
  standing = { kind: 'stale', approval: 90, revision: 1, changes: [{ pin: 'issue_revision', approved: 'sha256:asked', current: 'sha256:edited' }],
    diff: [{ part: 'Description', before: asked.description, after: edited.description,
      lines: [{ kind: 'same', text: 'Users export the unit list.' }, { kind: 'removed', text: 'As a spreadsheet.' },
        { kind: 'added', text: 'As XLSX, with a summary sheet.' }] }] };
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
          worktrees: ['/x/' + KEY], run: { flow: FLOW, worktree: '/x/' + KEY, state: flowView().state,
            openItems: inbox.length, start: 'ready' }, section: inbox.length > 0 ? 'needs_you' : 'running' }],
        readAt: Date.now(), failure: null, outdated: false };
      return cmd === 'sync_tracker' ? { queue, report: null } : queue;
    }
    case 'tracker_issue':
      return { tracker: 'jira', readAt: Date.now(), column: 1, role: 'started', account: null,
        detail: { issue: structuredClone(issue), description: issueNow.description, parent: null, updated: null,
          sprints: [sprint], links: [], fields: [] } };
    case 'issue_start':
      return { workItem: FLOW, flow: flowView(), job: { workItem: FLOW, flow: FLOW, project: '/x/webapp', key: KEY,
        candidate: { kind: 'new', branch: 'feature/' + KEY, directory: '/x/' + KEY, base: 'main' }, worktree: '/x/' + KEY,
        branch: 'feature/' + KEY, state: 'ready', detail: null, setupSession: null, createdAt: Date.now(), updatedAt: Date.now() },
        working: false, moves: [], inbox: structuredClone(inbox), startMove: { kind: 'stay', reason: 'already_there' },
        startStatus: 'In Progress' };
    case 'issue_plan': return planView();
    case 'question_thread': return thread();
    case 'discuss_question':
      discussion.push({ id: 1, question: 7, from: 'you', text: a.text, at: Date.now() });
      replying = true;
      changed();
      return null;
    case 'answer_question': {
      question.answer = { choice: a.choice, text: a.text, at: Date.now() };
      inbox.splice(0, inbox.length);
      plans.push({ id: 1, flow: FLOW, revision: 1, step: 3, invocation: 43, plan, issue: asked,
        issueRevision: 'sha256:asked', submittedAt: Date.now() });
      sessions.push(session(3, 'submitted', null));
      state.phase = 'planned';
      state.hold = { kind: 'awaiting_approval' };
      standing = { kind: 'waiting', revision: 1 };
      inbox.push({ id: 2, flow: FLOW, kind: 'approval', target: { kind: 'flow' },
        title: KEY + ': plan revision 1 is ready for your approval', body: plan.summary, state: 'open',
        createdAt: Date.now(), closedAt: null, project: '/x/webapp', worktree: '/x/' + KEY, issue: KEY, question: null });
      changed();
      setTimeout(() => emit('flow:inbox', inbox[0]), 0);
      return structuredClone(question);
    }
    case 'question_comment_draft':
      return { question: 7, tracker: 'jira', key: KEY,
        body: 'Question: CSV or XLSX for the export?\\nWhy it matters: The issue says "spreadsheet", and XLSX needs a new dependency.\\nAnswer: CSV. Keep the list\\'s column order.' };
    case 'post_question_comment': {
      const view = { approval: 80, request: { payload: { tracker: 'jira', key: KEY, body: a.body }, question: 7 },
        operation: { id: 80, key: { flow: FLOW, kind: 'post_comment', target: 'jira:' + KEY, payloadHash: 'h' }, approval: 80,
          position: 0, payload: { kind: 'post_comment', tracker: 'jira', key: KEY, body: a.body }, state: 'done', attempts: 1,
          lastError: null, result: { kind: 'commented', key: KEY, comment: '10001', author: 'You', adopted: false },
          attention: null, resolution: null, createdAt: Date.now(), updatedAt: Date.now() }, state: 'published' };
      comments.push(view);
      changed();
      return view;
    }
    case 'approve_plan':
      decisions.push({ id: 1, flow: FLOW, plan: 1, revision: a.revision, verdict: { kind: 'approved', approval: 90,
        skipDesign: a.skipDesign }, at: Date.now() });
      standing = { kind: 'approved', approval: 90, revision: a.revision, skipDesign: a.skipDesign };
      state.hold = null;
      inbox.splice(0, inbox.length);
      changed();
      return decisions.at(-1);
    case 'read_planned_issue': changed(); return issueNow;
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
            name: 'plan-probe',
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
        await queue(page).getByText('ACME-201').waitFor();
        return page;
    }

    const shot = (page, path) => page.screenshot({ path, animations: 'disabled' });
    const calls = (page, cmd) =>
        page.evaluate((c) => window.calls.filter((x) => x.cmd === c), cmd);
    const queue = (page) => page.getByRole('navigation', { name: 'Work queue' });
    const row = (page, key) => queue(page).locator('.c-work-row').filter({ hasText: key });
    const runSection = (page) => page.getByRole('region', { name: 'Run', exact: true });
    const panel = (page) => page.getByRole('tabpanel');
    const card = (page) => panel(page).locator('.c-question').first();

    for (const scheme of ['light', 'dark']) {
        const page = await open(`?theme=${scheme}`, scheme);

        // ── The Run section says the run waits for you, and leads to the Plan tab ───────────
        await row(page, 'ACME-201').click();
        await runSection(page)
            .getByText(/planning reads “Waiting for your answer”, with 1 question for you/)
            .waitFor();
        await runSection(page).getByRole('button', { name: 'Open the Plan tab' }).click();

        // ── The question, as the plan's §2.4 asks it, with Send to Jira off until answered ───
        await card(page).getByText('CSV or XLSX for the export?').waitFor();
        const asked = await card(page).innerText();
        for (const part of [
            'Why it matters',
            'Unblocks',
            'src/export.py:4',
            'Recommendation:',
            'the run waits for your answer, and no CLI is held open',
        ])
            // Labels are uppercased by CSS, which `innerText` reports.
            assert(
                asked.toLowerCase().includes(part.toLowerCase()),
                `the card says “${part}”:\n${asked}`,
            );
        assert.equal(
            await card(page).getByRole('button', { name: 'Send to Jira…' }).isDisabled(),
            true,
        );
        assert.equal(
            await card(page).getByRole('radio', { name: /CSV/ }).isChecked(),
            true,
            'the recommendation is one click from being yours',
        );
        await shot(page, join(shots, `plan-question-${scheme}.png`));

        // ── Discuss: your message, then the planner's reply; the question stays open ────────
        await card(page).getByRole('button', { name: 'Discuss first' }).click();
        await card(page)
            .getByRole('textbox', { name: /Ask planner about it/ })
            .fill('Does anything else use the writer?');
        await card(page).getByRole('button', { name: 'Send to planner' }).click();
        await card(page).getByText('replying in the same conversation…').waitFor();
        assert.deepEqual(
            (await calls(page, 'discuss_question')).map((c) => c.args),
            [{ question: 7, text: 'Does anything else use the writer?' }],
        );
        await page.evaluate(() => window.reply());
        await card(page)
            .getByText(/Two reports import the CSV writer/)
            .waitFor();
        assert.equal((await calls(page, 'answer_question')).length, 0, 'still unanswered');
        await shot(page, join(shots, `plan-discussed-${scheme}.png`));

        // ── The same question in the inbox, with the same thread; answering there ───────────
        await page
            .getByRole('button', { name: /^Inbox/ })
            .first()
            .click();
        const inboxCard = page.locator('.c-work-inbox .c-question');
        await inboxCard.getByText(/Two reports import the CSV writer/).waitFor();
        await shot(page, join(shots, `inbox-question-${scheme}.png`));
        await inboxCard
            .getByRole('textbox', { name: /Add a note/ })
            .fill("Keep the list's column order.");
        await inboxCard.getByRole('button', { name: 'Answer and resume' }).click();
        await page.waitForFunction(() =>
            window.calls.some((c) => c.cmd === 'answer_question'),
        );
        assert.deepEqual((await calls(page, 'answer_question'))[0].args, {
            question: 7,
            choice: 0,
            text: "Keep the list's column order.",
        });

        // ── The plan-ready item reads the plan; it is not an offer to pause the run ──────────
        const ready = page.locator('.c-inbox-card').filter({ hasText: 'plan revision 1' });
        await ready.waitFor();
        assert.equal(await ready.getByRole('button', { name: 'Pause the run' }).count(), 0);
        await shot(page, join(shots, `inbox-plan-ready-${scheme}.png`));
        await ready.getByRole('button', { name: 'Read the plan' }).click();

        // ── The plan, whole, with the decision ─────────────────────────────────────────────
        const planCard = panel(page).locator('.c-plan-card');
        await planCard.getByText('Plan · revision 1').waitFor();
        const shown = await planCard.innerText();
        for (const part of [
            'Acceptance criteria',
            'Columns follow the list',
            'Verification',
            'src/views/UnitList.svelte',
            'Design: needed',
            'waits for your decision',
        ])
            assert(
                shown.toLowerCase().includes(part.toLowerCase()),
                `the plan shows “${part}”:\n${shown}`,
            );
        for (const button of ['Approve', 'Approve and skip design', 'Request changes'])
            await planCard.getByRole('button', { name: button, exact: true }).waitFor();
        await shot(page, join(shots, `plan-ready-${scheme}.png`));

        // ── Send to Jira…: a draft, nothing posted; then Post sends exactly the edited text ──
        const answered = panel(page)
            .locator('.c-question')
            .filter({ hasText: 'Your answer' });
        await answered.getByRole('button', { name: 'Send to Jira…' }).click();
        const draft = answered.getByRole('textbox', { name: /Draft comment on ACME-201/ });
        await draft.waitFor();
        assert.match(
            await draft.inputValue(),
            /Answer: CSV\. Keep the list's column order\./,
        );
        assert.doesNotMatch(await draft.inputValue(), /Two reports/, 'no discussion in it');
        assert.equal(
            (await calls(page, 'post_question_comment')).length,
            0,
            'a draft posts nothing',
        );
        await shot(page, join(shots, `send-to-jira-draft-${scheme}.png`));
        await draft.fill((await draft.inputValue()) + '\n(From the plan for ACME-201.)');
        await answered.getByRole('button', { name: 'Post to ACME-201' }).click();
        await answered.getByText(/Posted to ACME-201 as a comment by You/).waitFor();
        const posted = await calls(page, 'post_question_comment');
        assert.equal(posted.length, 1);
        assert.match(posted[0].args.body, /\(From the plan for ACME-201\.\)$/);

        // ── Approve: the issue's revision the tab showed goes with it ──────────────────────
        await planCard.getByRole('button', { name: 'Approve', exact: true }).click();
        await planCard
            .getByText(/Revision 1 approved, for the issue as it reads now/)
            .waitFor();
        assert.deepEqual((await calls(page, 'approve_plan'))[0].args, {
            flow: 30,
            revision: 1,
            issueRevision: 'sha256:asked',
            skipDesign: false,
        });
        await shot(page, join(shots, `plan-approved-${scheme}.png`));

        // ── The issue is edited: Check the issue finds the approval stale, with the diff ─────
        await page.evaluate(() => window.editIssue());
        await panel(page).getByRole('button', { name: 'Check the issue' }).click();
        await planCard
            .getByText(/no longer covers what it approved: the issue was edited/)
            .waitFor();
        await planCard
            .locator('.c-plan-diff__line--removed')
            .getByText('As a spreadsheet.')
            .waitFor();
        await planCard
            .locator('.c-plan-diff__line--added')
            .getByText('As XLSX, with a summary sheet.')
            .waitFor();
        await shot(page, join(shots, `plan-stale-${scheme}.png`));

        await page.context().close();
    }

    console.log(`ok — screenshots in ${shots}`);
} finally {
    await browser?.close();
    await server.close();
}

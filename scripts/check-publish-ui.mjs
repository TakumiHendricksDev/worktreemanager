/**
 * Drive the real app's Work view with mocked IPC through publishing a teammate's pull request
 * review: the outcome sheet's Publish, its exact preview, approval, and every state of the
 * publication card from the D1 PublishPartial board.
 *
 * What it proves, in order: Publish is the sheet's choice when there are findings, with a
 * confirmed bug picked and a suggestion kept private; Approve and publish stays shut until a
 * preview of the draft as it stands, and an edit after the preview shuts it again; the preview
 * sends the draft as edited and shows every body with its marker; approving sends exactly the
 * previewed draft and its hash; the Reviews tab then shows the publish posting, then published
 * with a link; a partial publish names the half that is missing and offers Retry; your own
 * pending review in the way offers adding to it, opening it, retrying and leaving it; a head that
 * moved after approval offers Re-review and, behind a second click, publishing against the
 * reviewed commit, which approves the same text with the new head; a relaunch mid-publish shows
 * Checking GitHub with nothing to press; a head that moved before approval is refused in the
 * sheet with both choices; and the inbox's item about a publish opens the same card. No Tauri,
 * no `gh`: the engine behind these shapes is `src-tauri/tests/publish_review.rs`'s.
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
const shots = process.env.WTM_SHOTS || join(tmpdir(), 'wtm-publish');
mkdirSync(shots, { recursive: true });

/*
 * The page: the real `App`, Work on, pull request #4031 reviewed at FIRST with a confirmed bug and
 * a suggestion. `window.pub` is the review's publication as the mocked backend holds it, and
 * `window.forgeHead` the head the forge reports. A test changes them and emits `flow:changed`.
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
const FIRST = '1156f977ff6d12d592bc152987b9d9b378f6718d';
const SECOND = '9e0d1c2b3a4f5e6d7c8b9a0f1e2d3c4b5a6f7e8d';
const project = { id: '/x/webapp', name: 'webapp', root: '/x/webapp', usable: true, problem: null, trust: null };
const wt = (dir, branch) => ({
  id: '/x/' + dir, title: dir, subtitle: branch ?? '(detached)', path: '/x/' + dir, dirname: dir,
  branch, head: FIRST, isMain: dir === 'webapp', isBare: false, locked: null, prunable: null,
  dirty: false, untracked: 0, staged: 0, ahead: 0, behind: 0, issueKey: null, badges: [], links: [],
  table: [], env: [], browserHome: null });
const worktrees = [wt('webapp', 'main'), wt('webapp-pr-4031', null)];
const item = { id: 1, repo: 'acme/webapp', number: 4031, title: 'Export units as CSV', worktree: '/x/webapp-pr-4031', reviewingSince: null };
const repo = { host: 'github.com', owner: 'acme', name: 'webapp' };
window.forgeHead = FIRST;
const pr = () => ({
  number: 4031, url: 'https://github.com/acme/webapp/pull/4031', title: 'Export units as CSV',
  author: 'sam-rivera', state: 'open', draft: false,
  head: { repo: { host: 'github.com', owner: 'sam-rivera', name: 'webapp' }, branch: 'feature/csv-export', sha: window.forgeHead },
  base: { repo, branch: 'main', sha: null },
  crossRepository: true, maintainerCanModify: false, requestedReviewers: [{ name: 'dana-lee', team: false }],
  checks: { passed: 3, failed: 0, pending: 0, skipped: 0 } });
const detail = () => ({ item: structuredClone(item), pullRequest: pr(), checksState: 'passing',
  permission: 'write', canPush: true, baseIntegration: false, baseProtection: null,
  account: { host: 'github.com', login: 'dana-lee', version: '2.100.0' } });
const source = (role, provider, invocation) => ({ step: invocation, invocation, ordinal: 0, role, provider,
  model: provider === 'claude' ? 'sonnet' : null, workspace: 'snapshot', treeSha: 'aa11' });
const finding = (id, title, path, line, cls, role, provider, invocation) => ({ id, flow: 9,
  class: cls, severity: 'notable', location: { path, startLine: line, endLine: line },
  title, rationale: 'Each unit past the last is read as empty.', requirement: null, treeSha: 'aa11', dismissed: null, lines: true,
  evidence: [{ source: source(role, provider, invocation), reported: { title, severity: 'notable', class: cls.kind, body: 'x', path, line } }] });
const bug = finding(91, 'The export loop drops the last unit', 'src/export.py', 20,
  { kind: 'confirmed_bug', verification: 'ran the export on three units and got two rows' }, 'correctness', 'codex', 1);
const rename = finding(92, 'Rename rows to unit_rows', 'src/export.py', 18, { kind: 'suggestion' }, 'docs', 'claude', 2);
const step = (id, role, provider) => ({ id, stage: 0, role, provider, model: provider === 'claude' ? 'sonnet' : null,
  profile: 'reviewer', workspace: 'snapshot', status: 'done', failures: 0, retries: 1, standsInFor: null,
  detail: null, resetsAt: null, skillProblem: null,
  result: { kind: 'findings', payload: { findings: [{ title: 't', severity: 'notable', class: 'suggestion', body: 'x' }] } } });
window.pub = null;
const view = () => ({
  flow: { id: 9, state: { kind: 'review_pr', phase: 'consolidated', hold: null, end: null }, startedAt: 0, updatedAt: 0 },
  review: { flow: 9, project: '/x/webapp', worktree: '/x/webapp-pr-4031', worktreePath: '/x/webapp-pr-4031', recipe: 'standard', base: 'origin/main', snapshot: 1 },
  snapshot: { headSha: FIRST, treeSha: 'aa11', baseSha: 'c0ffee11' },
  steps: [step(1, 'correctness', 'codex'), step(2, 'docs', 'claude')],
  invocations: [], inbox: [], running: false,
  findings: [bug, rename],
  checks: [], expectedChecks: [], applied: { rules: [], sources: ['CLAUDE.md'], skills: [] },
  pullRequest: {
    item: 1, repo: 'acme/webapp', number: 4031,
    mirror: { item: 1, pullRequest: pr(), permission: 'write', readAt: 1 },
    reviewing: null, reviewedHead: FIRST,
    movedTo: window.forgeHead === FIRST ? null : window.forgeHead,
    pushToBranch: 'fork_without_maintainer_edits', earlier: [], outcomes: [],
    publications: window.pub ? [structuredClone(window.pub)] : [],
    routes: [],
  },
  fixes: [], leases: [], writers: [], screenshots: [], pendingStart: null,
});
const MARK1 = '<!-- wtm:3f9a1c2b4d5e6f70 -->';
const MARK2 = '<!-- wtm:0a1b2c3d4e5f6a7b -->';
const payload = (draft) => ({
  repo, number: 4031, commitId: FIRST, event: draft.event,
  comments: draft.comments.map((c) => ({ finding: c.finding, path: 'src/export.py', startLine: null, line: c.finding === 91 ? 20 : 18, body: c.body })),
  folded: [], summary: draft.summary });
const hashOf = (draft) => 'sha256:' + Array.from(JSON.stringify(draft)).reduce((h, ch) => (h * 31 + ch.charCodeAt(0)) >>> 0, 7).toString(16);
const op = (id, position, kind, state, extra = {}) => ({
  id, key: { flow: 9, kind, target: 'acme/webapp#4031', payloadHash: 'sha256:op' + id }, approval: 5, position,
  payload: kind === 'pending_review'
    ? { kind, review: window.approvedHash, repo, number: 4031, commitId: FIRST, comments: window.approvedReview.comments }
    : { kind, review: window.approvedHash, repo, number: 4031, commitId: FIRST, event: 'comment', body: window.approvedReview.summary },
  state, attempts: state === 'pending' ? 0 : 1, lastError: null, result: null, attention: null, resolution: null,
  createdAt: Date.now(), updatedAt: Date.now(), ...extra });
const posted = (adopted = false) => ({ kind: 'review', review: 77, nodeId: 'PRR_77', url: 'https://github.com/acme/webapp/pull/4031#pullrequestreview-77', adopted });
window.publication = (states, extra = {}) => ({
  approval: { id: 5, flow: 9, intent: 'post', binding: { payload: window.approvedHash, commit: FIRST, head: window.approvedHead ?? FIRST }, grantedAt: Date.now() },
  review: structuredClone(window.approvedReview),
  operations: [
    op(1, 0, 'pending_review', states[0], states[0] === 'done' ? { result: posted(extra.adopted) } : (extra.first ?? {})),
    op(2, 1, 'submit_review', states[1], states[1] === 'done' ? { result: posted() } : (extra.second ?? {})),
  ],
  state: extra.state, url: states[1] === 'done' ? posted().url : null });
window.inbox = [];
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
    case 'review_queue': return { forge: 'github', items: [structuredClone(item)] };
    case 'review_detail': return detail();
    case 'review_recipes': return [{ id: 'standard', label: 'Standard', roles: [
      { id: 'correctness', provider: 'codex', model: null, skills: [] }, { id: 'docs', provider: 'claude', model: 'sonnet', skills: [] }] }];
    case 'latest_review': return a.worktree === '/x/webapp-pr-4031' ? view() : null;
    case 'review_run': return view();
    case 'review_staleness':
      return { flow: 9, treeSha: 'aa11', headSha: FIRST, changed: false, paths: [], complete: true,
        staleFindings: [], staleChecks: [], affected: [], pullRequestHead: null, pullRequestHeadHere: true };
    case 'preview_review_publish': {
      const review = payload(a.draft);
      return { review, hash: hashOf(a.draft),
        comments: review.comments.map((c, i) => ({ ...c, body: c.body + '\\n\\n' + MARK1.replace(' -->', ' c' + i + ' -->') })),
        pendingBody: MARK1, body: (a.draft.summary ? a.draft.summary + '\\n\\n' : '') + MARK2,
        account: 'dana-lee', ownPullRequest: false, head: window.forgeHead, url: 'https://github.com/acme/webapp/pull/4031' };
    }
    case 'approve_review_publish': {
      if (window.forgeHead !== FIRST && a.againstHead !== window.forgeHead) {
        throw { kind: 'headMoved', message: '#4031 changed after you reviewed it.', detail: { reviewed: FIRST, current: window.forgeHead } };
      }
      if (a.hash !== hashOf(a.draft)) throw { kind: 'flow', message: 'What would post has changed since you previewed it.', detail: null };
      window.approvedReview = payload(a.draft);
      window.approvedHash = a.hash;
      window.approvedHead = a.againstHead ?? FIRST;
      window.pub = window.publication(['executing', 'pending'], { state: 'posting' });
      return structuredClone(window.pub);
    }
    case 'review_publication': return structuredClone(window.pub);
    case 'retry_publication':
    case 'add_to_pending_review':
      window.pub = window.publication(['done', 'executing'], { state: 'posting' });
      return structuredClone(window.pub);
    case 'cancel_publication':
      window.pub = window.publication(['done', 'cancelled'], { state: 'cancelled' });
      return structuredClone(window.pub);
    case 'open_url': return null;
    case 'work_inbox': return structuredClone(window.inbox);
    default: return cmd.startsWith('list_') ? [] : null;
  }
}, { shouldMockEvents: true });
await import('@xterm/xterm/css/xterm.css');
await import('/src/styles/main.scss');
const { mount } = await import('svelte');
const { default: App } = await import('/src/App.svelte');
const { workspace } = await import('/src/lib/state/workspace.svelte.ts');
window.harness = { emit, workspace, FIRST, SECOND, MARK1 };
mount(App, { target: document.getElementById('app') });`;

const server = await createServer({
    configFile: false,
    envDir: false,
    logLevel: 'warn',
    plugins: [
        svelte(),
        {
            name: 'publish-probe',
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

    const shot = (page, path) => page.screenshot({ path, animations: 'disabled' });
    const calls = (page, cmd) =>
        page.evaluate((c) => window.calls.filter((x) => x.cmd === c), cmd);
    const queue = (page) => page.getByRole('navigation', { name: 'Work queue' });
    const panel = (page) => page.locator('#work-pr-panel');
    const card = (page) => panel(page).locator('.c-publication');
    /** Put the backend's publication in `states` and tell the window the run moved. */
    async function publish(page, states, extra) {
        await page.evaluate(
            ([s, e]) => {
                window.pub = window.publication(s, e);
                return window.harness.emit('flow:changed', { flow: 9 });
            },
            [states, extra],
        );
    }
    async function cardSays(page, words) {
        await card(page).locator('.c-work-state__eyebrow', { hasText: words }).waitFor();
        return card(page).innerText();
    }

    for (const scheme of ['light', 'dark']) {
        const context = await browser.newContext({
            viewport: { width: 1280, height: 1000 },
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
        await queue(page).waitFor({ state: 'visible' });
        const { FIRST, SECOND } = await page.evaluate(() => ({
            FIRST: window.harness.FIRST,
            SECOND: window.harness.SECOND,
        }));
        await queue(page).getByRole('button', { name: /#4031/ }).click();
        await page.getByRole('tab', { name: 'Reviews', selected: true }).waitFor();

        // ── The sheet: Publish chosen, the bug picked, the suggestion kept private ──────────
        await panel(page).getByRole('button', { name: 'Finish review…' }).click();
        const sheet = page.getByRole('dialog', { name: 'Finish your review of #4031' });
        await sheet.waitFor();
        assert.equal(
            await sheet.getByRole('radio', { name: /Publish a review/ }).isChecked(),
            true,
        );
        const bug = sheet.getByRole('checkbox', { name: /drops the last unit/ });
        const rename = sheet.getByRole('checkbox', { name: /Rename rows/ });
        assert.equal(await bug.isChecked(), true, 'a confirmed bug starts picked');
        assert.equal(await rename.isChecked(), false, 'a suggestion starts private');
        await sheet.getByText('(kept private)').waitFor();
        const approve = sheet.getByRole('button', { name: 'Approve and publish' });
        assert.equal(
            await approve.isDisabled(),
            true,
            'nothing to approve before a preview',
        );

        const comment = sheet.getByRole('textbox', { name: 'Comment on src/export.py:20' });
        await comment.fill('This drops the last unit: `units[:-1]` skips it.');
        await sheet
            .getByRole('textbox', { name: 'Summary comment' })
            .fill('One bug before merge.');
        await sheet.getByRole('button', { name: 'Preview exactly what posts' }).click();
        const preview = sheet.getByRole('region', { name: 'What posts' });
        await preview.waitFor();
        const draft = {
            comments: [
                { finding: 91, body: 'This drops the last unit: `units[:-1]` skips it.' },
            ],
            summary: 'One bug before merge.',
            event: 'comment',
        };
        assert.deepEqual((await calls(page, 'preview_review_publish')).at(-1).args, {
            flow: 9,
            draft,
        });
        let text = await preview.innerText();
        for (const words of [
            'Exactly what posts as @dana-lee to acme/webapp #4031',
            `pinned to commit ${FIRST.slice(0, 7)}`,
            'src/export.py:20',
            '<!-- wtm:3f9a1c2b4d5e6f70 c0 -->',
            'One bug before merge.',
            'HTML comment GitHub does not show',
        ])
            assert(text.includes(words), `the preview says “${words}”:\n${text}`);
        assert.equal(await approve.isDisabled(), false);
        await shot(page, join(shots, `publish-preview-${scheme}.png`));

        // An edit after the preview is not what was previewed.
        await sheet
            .getByRole('textbox', { name: 'Summary comment' })
            .fill('One bug before merge, please.');
        await preview.getByText('You changed something since this preview').waitFor();
        assert.equal(await approve.isDisabled(), true);
        await sheet
            .getByRole('textbox', { name: 'Summary comment' })
            .fill('One bug before merge.');
        await sheet.getByRole('button', { name: 'Preview exactly what posts' }).click();
        await preview.getByText('Exactly what posts').waitFor();

        await approve.click();
        await sheet.waitFor({ state: 'detached' });
        const approved = (await calls(page, 'approve_review_publish')).at(-1).args;
        assert.deepEqual(approved.draft, draft, 'exactly the draft previewed');
        assert.equal(approved.againstHead, null);
        assert.match(approved.hash, /^sha256:/);

        // ── Posting, then published ──────────────────────────────────────────────────────────
        text = await cardSays(page, 'Publishing');
        assert(text.includes('Posting 1 inline comment…'), text);
        await publish(page, ['done', 'done'], { state: 'published' });
        text = await cardSays(page, 'Published');
        assert(text.includes('Review on #4031 published'), text);
        assert(text.includes('1 inline comment posted'), text);
        assert(text.includes('Summary and review state (Comment) posted'), text);
        await card(page).getByRole('button', { name: 'View on GitHub' }).first().click();
        assert.equal(
            (await calls(page, 'open_url')).at(-1).args.url,
            'https://github.com/acme/webapp/pull/4031#pullrequestreview-77',
        );
        await shot(page, join(shots, `publish-published-${scheme}.png`));

        // ── Partly published: the half that is missing, and Retry ────────────────────────────
        await publish(page, ['done', 'failed'], {
            state: 'failed',
            second: { lastError: 'GitHub could not be read, so nothing was sent.' },
        });
        text = await cardSays(page, 'Partly published');
        assert(text.includes('Review on #4031: 1 of 2 steps done'), text);
        assert(text.includes('Summary and review state (Comment) not posted'), text);
        assert(
            text.includes('Retry checks GitHub first and sends only what is missing'),
            text,
        );
        await shot(page, join(shots, `publish-partial-${scheme}.png`));
        await card(page).getByRole('button', { name: 'Retry' }).click();
        assert.deepEqual((await calls(page, 'retry_publication')).at(-1).args, {
            approval: 5,
        });

        // ── Your pending review in the way ─────────────────────────────────────────────────
        await publish(page, ['needs_attention', 'pending'], {
            state: 'needs_you',
            first: {
                attention: {
                    kind: 'pending_review',
                    review: 66,
                    nodeId: 'PRR_66',
                    url: 'https://github.com/acme/webapp/pull/4031#pullrequestreview-66',
                    comments: 1,
                    startedByWtm: false,
                },
            },
        });
        text = await cardSays(page, 'Needs you');
        assert(
            text.includes('You already have a pending review on #4031, holding 1 comment'),
            text,
        );
        assert(text.includes('it refuses a second (HTTP 422)'), text);
        assert(text.includes('along with the comment it already holds'), text);
        for (const name of [
            'Add to my pending review',
            'Open pending review on GitHub',
            'Retry',
            'Leave as is',
        ])
            await card(page).getByRole('button', { name }).waitFor();
        await shot(page, join(shots, `publish-pending-review-${scheme}.png`));
        await card(page)
            .getByRole('button', { name: 'Open pending review on GitHub' })
            .click();
        assert.equal(
            (await calls(page, 'open_url')).at(-1).args.url,
            'https://github.com/acme/webapp/pull/4031#pullrequestreview-66',
        );
        await card(page).getByRole('button', { name: 'Add to my pending review' }).click();
        assert.deepEqual((await calls(page, 'add_to_pending_review')).at(-1).args, {
            approval: 5,
        });

        // ── The head moved after approval: re-review, or publish against the commit anyway ──
        await page.evaluate((head) => (window.forgeHead = head), SECOND);
        await publish(page, ['done', 'needs_attention'], {
            state: 'stale',
            second: { attention: { kind: 'head_moved', approved: FIRST, current: SECOND } },
        });
        text = await cardSays(page, 'Partly published');
        assert(text.includes('#4031 changed after you approved'), text);
        assert(text.includes(`head now ${SECOND.slice(0, 7)}`), text);
        assert(
            text.includes(
                `moved to ${SECOND.slice(0, 7)} after you approved ${FIRST.slice(0, 7)}`,
            ),
            text,
        );
        await card(page).getByRole('button', { name: 'Re-review the changes' }).waitFor();
        await shot(page, join(shots, `publish-stale-${scheme}.png`));
        await card(page)
            .getByRole('button', { name: `Publish against ${FIRST.slice(0, 7)} anyway…` })
            .click();
        await card(page)
            .getByRole('button', {
                name: `Publish against ${FIRST.slice(0, 7)}`,
                exact: true,
            })
            .click();
        const again = (await calls(page, 'approve_review_publish')).at(-1).args;
        assert.equal(again.againstHead, SECOND, 'approved again, against the new head');
        assert.deepEqual(again.draft, draft, 'the same text');
        assert.equal(again.hash, approved.hash, 'the same hash');

        // ── After a relaunch mid-publish: checking GitHub, nothing to press ─────────────────
        await page.evaluate((head) => (window.forgeHead = head), FIRST);
        await publish(page, ['executing', 'pending'], { state: 'checking' });
        text = await cardSays(page, 'Checking GitHub');
        assert(text.includes('wtm stopped while posting to #4031'), text);
        assert(text.includes('1 inline comment: checking GitHub…'), text);
        assert(text.includes('Nothing is sent on its own'), text);
        assert.equal(await card(page).getByRole('button').count(), 0);
        await shot(page, join(shots, `publish-checking-${scheme}.png`));

        // ── A head that moved before approval is refused in the sheet, with both choices ─────
        await page.evaluate((head) => (window.forgeHead = head), SECOND);
        await page.evaluate(() => window.harness.emit('flow:changed', { flow: 9 }));
        await panel(page).getByRole('button', { name: 'Finish review…' }).click();
        await sheet.waitFor();
        await sheet.getByRole('button', { name: 'Preview exactly what posts' }).click();
        await sheet.getByRole('region', { name: 'What posts' }).waitFor();
        await sheet.getByRole('button', { name: 'Approve and publish' }).click();
        const stale = sheet.locator('.c-work-state', { hasText: 'Approval is stale' });
        await stale.waitFor();
        text = await stale.innerText();
        assert(text.includes('#4031 changed after you reviewed it'), text);
        assert(text.includes('Nothing was posted.'), text);
        await stale.getByRole('button', { name: 'Re-review the changes' }).waitFor();
        await stale
            .getByRole('button', { name: `Publish against ${FIRST.slice(0, 7)} anyway…` })
            .waitFor();
        await shot(page, join(shots, `publish-sheet-moved-${scheme}.png`));
        await sheet.getByRole('button', { name: 'Cancel' }).last().click();
        await sheet.waitFor({ state: 'detached' });

        // ── The inbox's item about a publish opens the same card ───────────────────────────
        await page.evaluate((head) => (window.forgeHead = head), FIRST);
        await page.evaluate(() => {
            window.pub = window.publication(['done', 'failed'], {
                state: 'failed',
                second: { lastError: 'error connecting to api.github.com' },
            });
            window.inbox = [
                {
                    id: 40,
                    flow: 9,
                    kind: 'failure',
                    target: { kind: 'approval', approval: 5 },
                    title: 'Your review of #4031 was only partly published',
                    body: 'error connecting to api.github.com',
                    state: 'open',
                    createdAt: Date.now(),
                    closedAt: null,
                    project: '/x/webapp',
                    worktree: '/x/webapp-pr-4031',
                    question: null,
                },
            ];
            // The engine announces both: the publish changed, and an item was filed about it.
            return window.harness
                .emit('flow:changed', { flow: 9 })
                .then(() => window.harness.emit('flow:inbox', window.inbox[0]));
        });
        await page.getByRole('button', { name: /^Inbox/ }).click();
        const inboxCard = page.locator('.c-inbox-card');
        await inboxCard.locator('.c-publication').waitFor();
        text = await inboxCard.innerText();
        assert(text.includes('Your review of #4031 was only partly published'), text);
        assert(text.includes('Review on #4031: 1 of 2 steps done'), text);
        await inboxCard.getByRole('button', { name: 'Show the review' }).waitFor();
        await shot(page, join(shots, `publish-inbox-${scheme}.png`));
        await inboxCard.getByRole('button', { name: 'Retry' }).click();
        assert.deepEqual((await calls(page, 'retry_publication')).at(-1).args, {
            approval: 5,
        });
        await context.close();
    }

    console.log(`ok — screenshots in ${shots}`);
} finally {
    await browser?.close();
    await server.close();
}

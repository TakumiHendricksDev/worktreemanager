/** Exercise the actual chat review card and preview controls with mocked IPC. Native loading
 * is covered separately by src-tauri/examples/prototype_preview.rs; this probe spends no credits. */
import assert from 'node:assert/strict';
import { pathToFileURL } from 'node:url';
import { createServer } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

if (!process.env.WTM_PLAYWRIGHT_MODULE)
    throw new Error('Set WTM_PLAYWRIGHT_MODULE to an existing Playwright module.');
const { chromium } = await import(pathToFileURL(process.env.WTM_PLAYWRIGHT_MODULE).href);
const fixture = `
window.checkState={calls:[],listeners:{},callbacks:{},next:0,failPreview:true};
const state=window.checkState;
const storageKey='wtm-prototype-probe:'+location.search;
const fresh=()=>({prototype:{id:'design-one',projectId:'/project',worktreeId:'/project/topic',root:'/project/topic',session:'same-chat',provider:'codex',conversation:'conversation-one',title:'Registration export',brief:'Prototype the export using our components',created:'2026-10-14',upstream:'pinned',sources:[{path:'src/ui/Button.vue',kind:'component',text:'<button class="btn btn-primary local-button">Export</button>',fingerprint:'source-1'},{path:'src/tokens.css',kind:'tokens',text:':root {--accent:#734399;}',fingerprint:'source-2'}],directory:'/isolated/design-one',revision:'review-one',phase:'ready for review',feedback:[],review:{id:'review-one',fingerprint:'html-1',opened:false},implementedReview:null,dismissed:false},stale:location.search.includes('stale')});
const saved=JSON.parse(localStorage.getItem(storageKey)||'null');
if(location.search.includes('several')){
 state.record=saved?.record??fresh();
 state.record.prototype.implementedReview??='review-one';
 state.older=saved?.older??{prototype:{...fresh().prototype,id:'design-older',title:'Older export',created:'2026-10-13',revision:'review-old',review:{id:'review-old',fingerprint:'html-old',opened:false}},stale:false};
}else{
 state.record=saved??fresh();
 state.older=null;
 if(location.search.includes('generating')){state.record.prototype.review=null;state.record.prototype.phase='generating';}
}
const persist=()=>localStorage.setItem(storageKey,JSON.stringify(location.search.includes('several')?{record:state.record,older:state.older}:state.record));
state.view={id:'preview',project:'/project',worktree:'/project/topic',url:'about:blank',prototypeId:'design-one/review-one',title:'Prototype review',loading:false,loadError:'The saved prototype could not be displayed. Choose Reload or Show preview to retry. The saved design is unchanged.',canGoBack:false,canGoForward:false,agentAccess:true,openedBy:'same-chat',commentMode:false,agentDriving:null};
window.emitDesign=()=>{for(const id of Object.values(state.listeners))state.callbacks[id]?.({id,event:'prototype:changed',payload:{session:'same-chat',state:structuredClone(state.record)}})};
window.__TAURI_INTERNALS__={metadata:{currentWindow:{label:'main'},currentWebview:{label:'main'}},transformCallback:fn=>{const id=++state.next;state.callbacks[id]=fn;return id;},unregisterCallback:id=>delete state.callbacks[id],invoke:async(command,args)=>{
 state.calls.push({command,args});
 if(command==='plugin:event|listen'){state.listeners[args.event]=args.handler;return ++state.next;}
 if(command==='plugin:event|unlisten')return;
 if(command==='list_prototypes')return location.search.includes('several')?[structuredClone(state.record),structuredClone(state.older)]:[structuredClone(state.record)];
 if(command==='open_prototype_preview'){if(state.failPreview)throw {kind:'preview',message:state.view.loadError};state.record.prototype.review.opened=true;window.emitDesign();return state.view;}
 if(command==='implement_prototype'){state.record.prototype.implementedReview=args.reviewId;persist();window.emitDesign();return;}
 if(command==='dismiss_prototype'){if(state.failDismiss)throw {kind:'preview',message:'Cannot save dismissal. Retry.'};const target=[state.record,state.older].find((item)=>item?.prototype.id===args.id)??state.record;target.prototype.dismissed=true;persist();return structuredClone(target);}
 if(command==='cancel_prototype')return;
 if(command==='close_browser')return;
 if(command.startsWith('browser_'))return;
 throw new Error('Unexpected IPC '+command);
}};`;
const entry = `
import {mount} from 'svelte';
import Card from '/src/lib/components/PrototypeReview.svelte';
import BrowserPane from '/src/lib/components/BrowserPane.svelte';
import {sessions} from '/src/lib/state/sessions.svelte.ts';
import {browsers} from '/src/lib/state/browsers.svelte.ts';
import '/src/styles/main.scss';
const s=window.checkState;
sessions.options=[{id:'codex',label:'Codex',available:true,offered:true}];
sessions.send=async(...args)=>{s.calls.push({command:'send-same-chat',args});return true;};
const pane={id:'origin-pane',session:'same-chat',projectId:'/project',worktreeId:'/project/topic',working:location.search.includes('generating'),ended:null,detached:false,kind:{kind:'agent',provider:'codex'}};
mount(Card,{target:document.querySelector('#card'),props:{pane}});
browsers.apply(s.view);
mount(BrowserPane,{target:document.querySelector('#preview'),props:{pane:{...pane,id:'browser-pane',session:'preview',kind:{kind:'browser'}},visible:true}});`;
const server = await createServer({
    configFile: false,
    envDir: false,
    plugins: [
        svelte(),
        {
            name: 'chat-prototype-probe',
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
                        '<!doctype html><html data-theme="light" data-palette="iris"><body><main style="padding:24px;display:grid;grid-template-columns:1fr 1fr;gap:24px"><div id="card"></div><div id="preview" style="height:650px;display:flex;flex-direction:column"></div></main><script>' +
                            fixture +
                            '</script><script type="module" src="/probe-entry.js"></script></body></html>',
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
    const page = await browser.newPage({ viewport: { width: 1180, height: 850 } });
    const errors = [];
    page.on('pageerror', (e) => {
        errors.push(e.message);
        console.error('PAGE ERROR', e.message);
    });
    page.on('console', (message) => {
        if (message.type() === 'error') console.error('CONSOLE', message.text());
    });
    const url = `http://127.0.0.1:${server.httpServer.address().port}/probe`;
    await page.goto(url);
    await page.getByRole('heading', { name: 'Registration export' }).waitFor();
    const calls = () => page.evaluate(() => window.checkState.calls);
    const card = page.locator('#card .c-design-first');
    async function assertHiddenOnReload() {
        await page.reload();
        await page.getByRole('button', { name: 'Close preview', exact: true }).waitFor();
        // The list call is recorded before Svelte applies it. Wait until a frame after
        // that apply, so an empty first paint cannot pass for a card that comes back.
        await page.waitForFunction(async () => {
            if (!window.checkState?.calls.some((c) => c.command === 'list_prototypes'))
                return false;
            await new Promise((resolve) =>
                requestAnimationFrame(() => requestAnimationFrame(resolve)),
            );
            return document.querySelector('#card .c-design-first') === null;
        });
        assert.equal(await card.count(), 0);
        assert(
            !(await calls()).some((c) =>
                [
                    'implement_prototype',
                    'open_prototype_preview',
                    'dismiss_prototype',
                    'cancel_prototype',
                    'send-same-chat',
                    'close_browser',
                ].includes(c.command),
            ),
            'Restoring a hidden card must not replay any action',
        );
    }
    assert(
        !(await calls()).some((c) =>
            ['implement_prototype', 'open_prototype_preview', 'send-same-chat'].includes(
                c.command,
            ),
        ),
        'Restoring must not replay actions',
    );
    assert(
        await page
            .getByRole('button', { name: 'Implement this design', exact: true })
            .isDisabled(),
    );
    await page.getByRole('button', { name: 'Show preview', exact: true }).click();
    await page
        .getByRole('alert')
        .filter({ hasText: 'The saved prototype could not be displayed' })
        .last()
        .waitFor();
    assert(!(await calls()).some((c) => c.command === 'implement_prototype'));
    await page.screenshot({
        path: '/tmp/wtm-chat-prototype-preview-error.png',
        fullPage: true,
    });
    await page.evaluate(() => (window.checkState.failPreview = false));
    await page.getByRole('button', { name: 'Show preview', exact: true }).click();
    await page.waitForFunction(() => window.checkState.record.prototype.review.opened);
    const beforeClose = (await calls()).length;
    const close = page.getByRole('button', { name: 'Close preview', exact: true });
    assert.equal((await close.textContent()).trim(), 'Close');
    await close.click();
    assert.deepEqual(
        (await calls()).slice(beforeClose),
        [{ command: 'close_browser', args: { id: 'preview' } }],
        'Closing the preview must not end the originating conversation or ask for confirmation',
    );
    const opened = (await calls()).filter(
        (c) => c.command === 'open_prototype_preview',
    ).length;
    await page.getByRole('button', { name: 'Show preview', exact: true }).click();
    assert.equal(
        (await calls()).filter((c) => c.command === 'open_prototype_preview').length,
        opened + 1,
    );
    await page
        .getByLabel('Feedback for this design', { exact: true })
        .fill('Make the field list compact');
    assert(
        await page
            .getByRole('button', { name: 'Implement this design', exact: true })
            .isDisabled(),
    );
    await page.getByRole('button', { name: 'Revise in this chat', exact: true }).click();
    const sent = (await calls()).find((c) => c.command === 'send-same-chat');
    assert.equal(sent.args[0], 'origin-pane');
    assert(sent.args[1].includes('design-one'));
    await page.evaluate(() => {
        const p = window.checkState.record.prototype;
        p.revision = 'review-two';
        p.review = { id: 'review-two', fingerprint: 'html-two', opened: false };
        window.emitDesign();
    });
    assert(
        await page
            .getByRole('button', { name: 'Implement this design', exact: true })
            .isDisabled(),
    );
    await page.getByRole('button', { name: 'Show preview', exact: true }).click();
    await page.getByRole('button', { name: 'Implement this design', exact: true }).click();
    const implemented = (await calls()).filter((c) => c.command === 'implement_prototype');
    assert.equal(implemented.length, 1);
    assert.deepEqual(implemented[0].args, {
        session: 'same-chat',
        id: 'design-one',
        reviewId: 'review-two',
    });
    await card.waitFor({ state: 'detached' });
    await assertHiddenOnReload();
    assert.equal(
        await page.evaluate(() => window.checkState.record.prototype.implementedReview),
        'review-two',
    );
    await page.goto(url + '?stale');
    await page.getByRole('heading', { name: 'Registration export' }).waitFor();
    assert(
        await page
            .getByRole('button', { name: 'Implement this design', exact: true })
            .isDisabled(),
    );
    await page.getByRole('button', { name: 'Refresh and revise', exact: true }).waitFor();
    await page.goto(url + '?dismiss');
    await page.getByRole('heading', { name: 'Registration export' }).waitFor();
    await page.evaluate(() => {
        window.checkState.failPreview = false;
        window.checkState.failDismiss = true;
    });
    await page.getByRole('button', { name: 'Show preview', exact: true }).click();
    assert(
        await page
            .getByRole('button', { name: 'Implement this design', exact: true })
            .isEnabled(),
    );
    const dismiss = page.getByRole('button', {
        name: 'Dismiss design review',
        exact: true,
    });
    await dismiss.click();
    await page
        .getByRole('alert')
        .filter({ hasText: 'Cannot save dismissal. Retry.' })
        .waitFor();
    assert.equal(await card.count(), 1);
    await page.evaluate(() => (window.checkState.failDismiss = false));
    const beforeDismiss = (await calls()).length;
    await dismiss.click();
    await card.waitFor({ state: 'detached' });
    assert.deepEqual(
        (await calls()).slice(beforeDismiss),
        [
            {
                command: 'dismiss_prototype',
                args: { session: 'same-chat', id: 'design-one' },
            },
        ],
        'Dismissing the card must only save its visibility',
    );
    await assertHiddenOnReload();
    assert.equal(
        await page.evaluate(() => window.checkState.record.prototype.review.id),
        'review-one',
    );
    assert.equal(
        await page.evaluate(() => window.checkState.record.prototype.implementedReview),
        null,
    );
    await page.goto(url + '?generating');
    await page.getByText('Creating your prototype…', { exact: true }).waitFor();
    const generatingCalls = (await calls()).length;
    await dismiss.click();
    await card.waitFor({ state: 'detached' });
    assert.deepEqual((await calls()).slice(generatingCalls), [
        { command: 'dismiss_prototype', args: { session: 'same-chat', id: 'design-one' } },
    ]);
    await page.evaluate(() => {
        window.checkState.record.prototype.review = {
            id: 'review-one',
            fingerprint: 'html-1',
            opened: false,
        };
        window.checkState.record.prototype.phase = 'ready for review';
        window.emitDesign();
    });
    assert.equal(
        await card.count(),
        0,
        'A finished generation must not resurrect a dismissed card',
    );
    await assertHiddenOnReload();
    await page.goto(url + '?several');
    await page.getByRole('heading', { name: 'Older export' }).waitFor();
    assert.equal(await page.getByRole('heading', { name: 'Registration export' }).count(), 0);
    await page.getByRole('button', { name: 'Show preview', exact: true }).waitFor();
    await page.getByRole('button', { name: 'Implement this design', exact: true }).waitFor();
    const beforeOlder = (await calls()).length;
    await page.getByRole('button', { name: 'Dismiss design review', exact: true }).click();
    await card.waitFor({ state: 'detached' });
    assert.deepEqual(
        (await calls()).slice(beforeOlder),
        [
            {
                command: 'dismiss_prototype',
                args: { session: 'same-chat', id: 'design-older' },
            },
        ],
        'Dismissing one design must leave the other saved and must not close anything else',
    );
    await assertHiddenOnReload();
    assert.deepEqual(errors, []);
    console.log(
        'PASS: chat review, persisted card dismissal and approval hiding, preview Close and reopen, retryable errors, no action replay, same-chat revisions, exact approval, stale context, and a remaining design.',
    );
} finally {
    await browser?.close();
    await server.close();
}

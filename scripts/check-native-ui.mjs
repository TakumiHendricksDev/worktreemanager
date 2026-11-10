/** Actual native-session Svelte regression with explicitly mocked IPC. No provider requests.
 * Start the Vite dev server (`bun x --bun vite`, which serves on 5173) and an isolated headless
 * Chrome with a remote debugging port and a fresh profile, then:
 * WTM_CDP_PORT=9393 [WTM_DEV_URL=http://localhost:5173] bun scripts/check-native-ui.mjs
 * Optional WTM_SHOTS writes screenshots. The probe opens and closes only its own browser tab.
 *
 * Nothing here sleeps for a fixed time: every step waits for the state it needs, up to a few
 * seconds. Where a check asserts that something did not happen, it first waits for a sign that
 * the step before it took effect, so it cannot pass by running too early. Focus is emulated, so
 * Escape and focus assertions see the same events in a background headless tab as on screen.
 * Reconnect runs the real `sessions.restart` against mocked IPC; only Rust is simulated.
 */
import fs from 'node:fs/promises';
const port = process.env.WTM_CDP_PORT;
if (!port)
    throw Error(
        'Set WTM_CDP_PORT to an isolated Chrome remote-debugging port. Start the project dev server first.',
    );
const dev = process.env.WTM_DEV_URL ?? 'http://localhost:5173';
const page = await (
    await fetch(`http://127.0.0.1:${port}/json/new?about:blank`, { method: 'PUT' })
).json();
const ws = new WebSocket(page.webSocketDebuggerUrl);
await new Promise((r) => ws.addEventListener('open', r, { once: true }));
let seq = 0;
const pending = new Map(),
    errors = [];
ws.onmessage = ({ data }) => {
    const m = JSON.parse(data);
    if (m.id) {
        const p = pending.get(m.id);
        pending.delete(m.id);
        m.error ? p.reject(m.error) : p.resolve(m.result);
    }
    if (m.method === 'Runtime.exceptionThrown') errors.push(m.params.exceptionDetails);
};
const call = (method, params = {}) =>
    new Promise((resolve, reject) => {
        const id = ++seq;
        pending.set(id, { resolve, reject });
        ws.send(JSON.stringify({ id, method, params }));
    });
const evaluate = async (expression) => {
    const r = await call('Runtime.evaluate', {
        expression,
        returnByValue: true,
        awaitPromise: true,
    });
    if (r.exceptionDetails) throw r.exceptionDetails;
    return r.result.value;
};
/** Until `expression` is true, or fail after `timeout` ms with the last thing it returned. */
const settle = async (expression, label, timeout = 5000) => {
    const deadline = Date.now() + timeout;
    for (;;) {
        let last;
        try {
            last = await evaluate(expression);
        } catch (e) {
            last = e?.exception?.description ?? e?.text ?? String(e);
        }
        if (last === true) return;
        if (Date.now() > deadline)
            throw Error(`${label}: still ${JSON.stringify(last)} after ${timeout} ms`);
        await new Promise((r) => setTimeout(r, 50));
    }
};
const button = (text) =>
    `([...([...document.querySelectorAll('[role=dialog]')].at(-1)||document).querySelectorAll('button')].find(b=>b.textContent.trim()===${JSON.stringify(text)}))`;
/** Click a button in the top dialog, or the page, once it is there and enabled. */
const click = async (text) => {
    await settle(`(()=>{const b=${button(text)};return !!b&&!b.disabled})()`, `Button ${text}`);
    await evaluate(`${button(text)}.click()`);
};
const check = async (name, expression) => {
    await settle(expression, name);
    console.log(name, true);
};
const reviewHidden = `document.querySelector('#native-design-pane-1')?.hidden===true`;
const shot = async (name) => {
    if (!process.env.WTM_SHOTS) return;
    await fs.mkdir(process.env.WTM_SHOTS, { recursive: true });
    await fs.writeFile(
        process.env.WTM_SHOTS + '/' + name + '.png',
        Buffer.from(
            (await call('Page.captureScreenshot', { format: 'png' })).data,
            'base64',
        ),
    );
};
const escape = async () => {
    for (const type of ['keyDown', 'keyUp'])
        await call('Input.dispatchKeyEvent', {
            type,
            key: 'Escape',
            code: 'Escape',
            windowsVirtualKeyCode: 27,
            nativeVirtualKeyCode: 27,
        });
};
/** The text of the banner whose text includes `needle`, or null. Notices are `ui/Banner`s. */
const banner = (needle) =>
    `([...document.querySelectorAll('.c-native .c-banner')].find(b=>b.innerText.includes(${JSON.stringify(needle)}))??null)`;
/** Click a button inside the banner that mentions `needle`. */
const clickIn = async (needle, text) => {
    const target = `(${banner(needle)}?[...${banner(needle)}.querySelectorAll('button')].find(x=>x.textContent.trim()===${JSON.stringify(text)}):null)`;
    await settle(`(()=>{const b=${target};return !!b&&!b.disabled})()`, `${text} in ${needle}`);
    await evaluate(`${target}.click()`);
};
const implementButton = `[...document.querySelectorAll('.c-design-first button')].find(b=>b.textContent.trim()==='Implement this design')`;

try {
    await call('Runtime.enable');
    await call('Page.enable');
    await call('Emulation.setFocusEmulationEnabled', { enabled: true });
    await call('Emulation.setDeviceMetricsOverride', {
        width: 1200,
        height: 900,
        deviceScaleFactor: 1,
        mobile: false,
    });
    await call('Page.navigate', { url: dev + '/scripts/fixtures/native/index.html' });
    await settle(
        `!!window.qa?.sessions&&document.querySelector('.xterm')!==null&&qa.calls.some(c=>c.command==='terminal_replay')`,
        'The native fixture mounted its terminal',
        30000,
    );
    await check(
        'The native pane opens to the terminal without notices, activity or status chrome',
        `document.querySelector('.xterm')!==null&&document.querySelector('#native-design-pane-1').hidden&&!document.querySelector('.c-native .c-banner')&&!document.querySelector('.c-pane__note')&&!document.body.innerText.includes('Activity')&&!document.body.innerText.includes('Session details')`,
    );
    await check(
        'The terminal uses the full available width and height',
        `document.querySelector('.c-terminal').getBoundingClientRect().width>1100&&document.querySelector('.c-terminal').getBoundingClientRect().height>650`,
    );
    await check(
        'Open Design belongs to the existing pane header, as a disclosure and not a toggle',
        `(()=>{const b=[...document.querySelector('.c-pane__head').querySelectorAll('button')].find(b=>b.textContent.trim()==='Open Design');return document.querySelectorAll('.c-pane__head').length===1&&!!b&&b.getAttribute('aria-expanded')==='false'&&!b.hasAttribute('aria-pressed')})()`,
    );
    await shot('native-dark');
    const menu = async (label, choice) => {
        await settle(
            `!!document.querySelector('button[aria-label=${JSON.stringify(label)}]')`,
            `Menu ${label}`,
        );
        await evaluate(
            `qa.menuChoice=${JSON.stringify(choice)};document.querySelector('button[aria-label=${JSON.stringify(label)}]').click()`,
        );
    };
    await menu('Native session menu', 'Connection info…');
    await check(
        'A wide native pane menu holds only what has no button on screen',
        `!qa.menu.items.some(item=>['Split Right','Reconnect Session…','Pop Out into Its Own Window'].includes(item.text))&&qa.menu.items.some(item=>item.text==='New-session defaults…')`,
    );
    await check(
        'Connection info names the provider, its version separately, and no send or stop control the pane lacks',
        `(()=>{const d=document.querySelector('[role=dialog]').innerText;return d.includes('Connection info')&&d.includes('Native CLI')&&/Provider\\s*Codex\\b/.test(d)&&d.includes('Codex · UI fixture')&&!d.includes('Supported by this adapter')&&d.includes('Stopping a turn')})()`,
    );
    await escape();
    await check('Escape closes connection info', `!document.querySelector('[role=dialog]')`);
    await click('Open Design');
    await check(
        'Open Design has an empty state before a review exists',
        `document.body.innerText.includes('No design to review yet')&&!document.querySelector('#native-design-pane-1').hidden`,
    );
    await click('Hide');
    const replays = await evaluate(
        `qa.calls.filter(c=>c.command==='terminal_replay').length`,
    );
    await evaluate(`qa.publishReview()`);
    await check(
        'A published review opens its previously hidden region',
        `!document.querySelector('#native-design-pane-1').hidden&&document.body.innerText.includes('Example navigation')`,
    );
    await shot('native-review-dark');
    await evaluate(
        `(()=>{const input=document.querySelector('.c-design-first__feedback textarea');input.value='Keep this feedback';input.dispatchEvent(new Event('input',{bubbles:true}))})()`,
    );
    await click('Hide');
    await evaluate(`qa.update({status:'working'})`);
    await settle(
        `document.querySelector('.c-pane__note')?.innerText.trim()==='Working'`,
        'The status update rendered',
    );
    await check(
        'Status updates do not reopen a hidden review or duplicate the terminal',
        `document.querySelector('#native-design-pane-1').hidden&&qa.calls.filter(c=>c.command==='terminal_replay').length===${replays}`,
    );
    await check(
        'The header says the native status in words, as other panes do',
        `document.querySelector('.c-pane__note')?.innerText.trim()==='Working'`,
    );
    await check(
        'A hidden review waiting on the user is announced in a banner, not reopened',
        `${banner('Design ready for your review')}!==null`,
    );
    await clickIn('Design ready for your review', 'View review');
    await check(
        'View review opens the panel and moves focus into it',
        `!document.querySelector('#native-design-pane-1').hidden&&document.querySelector('#native-design-pane-1').contains(document.activeElement)`,
    );
    await check(
        'Hiding the review preserves its feedback',
        `document.querySelector('.c-design-first__feedback textarea').value==='Keep this feedback'`,
    );
    await evaluate(
        `(()=>{const input=document.querySelector('.c-design-first__feedback textarea');input.value='';input.dispatchEvent(new Event('input',{bubbles:true}))})()`,
    );
    await check(
        'A busy native turn cannot accept approval, and says why',
        `${implementButton}.disabled&&document.querySelector('.c-design-first').innerText.includes('Wait for the current turn')`,
    );
    await evaluate(`qa.update({status:'idle'});qa.publishReview({stale:true})`);
    // Implement was already disabled for the busy turn, so wait for that reason to go first.
    await settle(
        `!document.querySelector('.c-design-first').innerText.includes('Wait for the current turn')`,
        'The busy reason cleared',
    );
    await check('A stale review cannot accept approval', `${implementButton}.disabled`);
    await evaluate(`qa.publishReview({stale:false})`);
    await check(
        'An approval waits for the preview it approves, and says so',
        `${implementButton}.disabled&&document.querySelector('.c-design-first').innerText.includes('Show the preview first')`,
    );
    await click('Show preview');
    await check('Opening the preview enables Implement', `!${implementButton}.disabled`);
    await evaluate(`document.querySelector('.xterm-helper-textarea').focus()`);
    await escape();
    await check(
        'Escape in the native terminal leaves the review open',
        `!document.querySelector('#native-design-pane-1').hidden&&qa.calls.some(c=>c.command==='pty_write'&&c.args.dataBase64==='Gw==')`,
    );
    await evaluate(`document.querySelector('#native-design-pane-1 button').focus()`);
    await escape();
    await check(
        'Escape in the review returns focus to its header control',
        `document.querySelector('#native-design-pane-1').hidden&&document.activeElement.textContent.trim()==='Open Design'`,
    );
    await click('Open Design');
    await click('Implement this design');
    await check(
        'The exact review is dispatched once',
        `qa.calls.filter(c=>c.command==='implement_prototype').length===1&&qa.calls.find(c=>c.command==='implement_prototype').args.reviewId==='review'`,
    );
    await check(
        'A delivered approval says Delivered and cannot be clicked again',
        `(()=>{const b=[...document.querySelectorAll('.c-design-first button')].find(b=>b.textContent.trim()==='Delivered to this conversation');return !!b&&b.disabled&&document.querySelector('.c-design-first__meta').innerText.includes('Delivered to this conversation')})()`,
    );
    await click('Hide');
    await settle(reviewHidden, 'The review is hidden');
    await check(
        'A delivered design is not presented as ready for review',
        `${banner('Design ready')}===null&&${banner('Implement reviewed design')}===null`,
    );

    // Claude collects an approval on the next message. The review is offered again, as it would be
    // in a conversation that had not implemented it.
    await evaluate(
        `qa.update({provider:'claude',capabilities:{send:false,stop:false,plans:false,usage:false,permissionOwner:'native_cli'},deliveries:[]});qa.publishReview({prototype:{...qa.review.prototype,implementedReview:null},delivery:null})`,
    );
    await click('Open Design');
    await check(
        'Claude’s Implement copy says it is collected, not sent',
        `document.querySelector('.c-design-first').innerText.includes('collects it with the next message')`,
    );
    await click('Implement this design');
    await click('Hide');
    await check(
        'Claude continuation remains visible when the review is hidden, with Withdraw',
        `(()=>{const b=${banner('Implement reviewed design')};return !!b&&b.innerText.includes('Your next message in the terminal collects it')&&[...b.querySelectorAll('button')].some(x=>x.textContent.trim()==='Withdraw')&&qa.native.states[qa.current()].deliveries[0].status==='pending'})()`,
    );
    const sentBefore = await evaluate(
        `qa.calls.filter(c=>c.command==='implement_prototype').length`,
    );
    await clickIn('Implement reviewed design', 'Withdraw');
    await check(
        'Withdraw settles the receipt without sending anything',
        `qa.calls.some(c=>c.command==='resolve_native_delivery'&&c.args.resolution==='withdrawn')&&qa.calls.filter(c=>c.command==='implement_prototype').length===${sentBefore}&&${banner('Implement reviewed design')}===null`,
    );
    await click('Open Design');
    await check(
        'A withdrawn approval offers Implement again',
        `!!${implementButton}&&!${implementButton}.disabled`,
    );
    await click('Implement this design');
    await click('Hide');
    await check(
        'Implementing again leaves exactly one pending receipt',
        `qa.native.states[qa.current()].deliveries.filter(r=>r.status==='pending').length===1`,
    );
    await evaluate(
        `qa.update({status:'disconnected',error:null,deliveries:qa.native.states[qa.current()].deliveries.map(r=>r.status==='pending'?{...r,status:'unknown'}:r)})`,
    );
    await check(
        'A lost connection is not reported as a failed session',
        `qa.sessions.statusOfPane(qa.sessions.panes[0])==='attention'&&document.querySelector('.c-pane__note')?.innerText.trim().toLowerCase()==='status unknown · wtm connection lost'`,
    );
    const sent = await evaluate(
        `qa.calls.filter(c=>c.command==='implement_prototype').length`,
    );
    const before = await evaluate(`qa.current()`);
    const opened = await evaluate(`qa.calls.filter(c=>c.command==='open_native_cli').length`);
    const conversation = await evaluate(`qa.sessions.panes[0].providerSession`);
    await clickIn('lost its connection', 'Reconnect');
    await check(
        'Reconnect while the CLI may be mid-turn asks first, with native copy',
        `(()=>{const d=document.querySelector('[role=dialog]');return !!d&&d.innerText.includes('Reconnect session?')&&d.innerText.includes('resumes this conversation')&&d.innerText.includes('not resent')&&!qa.calls.some(c=>c.command==='close_terminal')&&qa.calls.filter(c=>c.command==='open_native_cli').length===${opened}})()`,
    );
    await click('Reconnect');
    await check(
        'Confirming closes the old process once and resumes its conversation in the same pane',
        `(()=>{const resumed=qa.calls.filter(c=>c.command==='open_native_cli').slice(${opened});return qa.calls.filter(c=>c.command==='close_terminal'&&c.args.session===${JSON.stringify(before)}).length===1&&resumed.length===1&&resumed[0].args.resume===${JSON.stringify(conversation)}&&resumed[0].args.integrated===true&&qa.current()!==${JSON.stringify(before)}&&!document.querySelector('[role=dialog]')})()`,
    );
    await check(
        'Reconnecting keeps the uncertain receipt uncertain and sends nothing again',
        `(()=>{const state=qa.native.states[qa.current()];return !!state&&state.conversation===${JSON.stringify(conversation)}&&state.deliveries.at(-1)?.status==='unknown'&&qa.calls.filter(c=>c.command==='implement_prototype').length===${sent}&&!qa.calls.some(c=>c.command==='send_native_message')})()`,
    );
    const resumedSession = await evaluate(`qa.current()`);
    await clickIn('Implement reviewed design', 'Check delivery');
    await check(
        'Check delivery says what it could not check, and replays nothing',
        `${banner('wtm cannot read Claude')}!==null&&qa.calls.filter(c=>c.command==='implement_prototype').length===${sent}&&qa.native.states[qa.current()].deliveries.at(-1).status==='unknown'`,
    );
    await check(
        'Checking delivery completes without an IPC error',
        `!document.querySelector('.c-native .c-banner--error')`,
    );
    await shot('native-pending-dark');
    await clickIn('Implement reviewed design', 'Mark not delivered');
    await check(
        'Mark not delivered settles the unknown receipt without resending',
        `qa.calls.some(c=>c.command==='resolve_native_delivery'&&c.args.resolution==='not_delivered')&&qa.calls.filter(c=>c.command==='implement_prototype').length===${sent}&&${banner('Implement reviewed design')}===null`,
    );
    await evaluate(
        `qa.update({deliveries:[{id:'earlier-receipt',label:'Earlier instruction',status:'pending',error:null,prototype:null,review:null,resolution:null,conversation:'old-conversation',earlier:true}]})`,
    );
    await check(
        'A receipt from an earlier conversation is listed and marked as such',
        `(()=>{const b=${banner('Earlier instruction')};return !!b&&b.innerText.includes('From an earlier conversation')&&[...b.querySelectorAll('button')].some(x=>x.textContent.trim()==='Withdraw')&&![...b.querySelectorAll('button')].some(x=>x.textContent.trim()==='Focus terminal')})()`,
    );
    await clickIn('Earlier instruction', 'Withdraw');
    await evaluate(
        `qa.update({status:'unavailable',error:'Example adapter failed to connect'})`,
    );
    await clickIn('Example adapter failed', 'Connection info');
    await click('Open plain terminal');
    await check(
        'Plain fallback opens a separate session and retains the original',
        `qa.calls.some(c=>c.command==='open_native_cli'&&c.args.integrated===false)&&qa.sessions.panes[0].session===${JSON.stringify(resumedSession)}&&qa.sessions.panes.length===2`,
    );
    await check(
        'The plain pane is titled apart from the integrated one',
        `qa.sessions.labelOf(qa.sessions.panes[1])==='Codex · CLI · plain'&&qa.sessions.labelOf(qa.sessions.panes[0])==='Codex · CLI'`,
    );
    await evaluate(
        `qa.update({status:'idle',error:null,deliveries:[]});qa.publishReview({prototype:{...qa.review.prototype,implementedReview:'review'}})`,
    );
    await check(
        'A completed review clears its reminder',
        `!document.querySelector('.c-native .c-banner')`,
    );
    await call('Emulation.setDeviceMetricsOverride', {
        width: 760,
        height: 900,
        deviceScaleFactor: 1,
        mobile: false,
    });
    await evaluate(`document.documentElement.dataset.theme='light';qa.theme.applied++`);
    await shot('native-light');
    await click('Open Design');
    await evaluate(
        `qa.publishReview({prototype:{...qa.review.prototype,implementedReview:null,review:{...qa.review.prototype.review,id:'review-2',opened:false}},delivery:null})`,
    );
    await click('Hide');
    await evaluate(
        `qa.publishReview({prototype:{...qa.review.prototype,implementedReview:null,review:{...qa.review.prototype.review,id:'review-3',opened:false}},delivery:null})`,
    );
    await check(
        'A new immutable revision is revealed',
        `!document.querySelector('#native-design-pane-1').hidden`,
    );
    await shot('native-review-light');
    await check(
        'The narrow review and terminal fit without overflow',
        `document.documentElement.scrollWidth<=760&&document.querySelector('.c-terminal').getBoundingClientRect().height>400`,
    );
    await click('New agent session');
    const radios = `[...document.querySelectorAll('input[name=experience]')]`;
    const choose = (provider) =>
        `(()=>{const select=document.querySelector('#native-provider');select.value=${JSON.stringify(provider)};select.dispatchEvent(new Event('change',{bubbles:true}))})()`;
    await check(
        'All three new-session experiences remain explicit',
        `${radios}.length===3`,
    );
    await evaluate(choose('cursor'));
    await check(
        'An agent without a native adapter falls back to WTM Chat and says why Native CLI is off',
        `(()=>{const r=${radios};const d=document.querySelector('[role=dialog]').innerText;return r[0].disabled&&r[2].disabled&&r[1].checked&&!r[1].disabled&&d.includes('not Cursor Agent')&&![...document.querySelectorAll('[role=dialog] button')].find(b=>b.textContent.trim()==='Open session').disabled})()`,
    );
    await evaluate(`qa.plugins.state.registry.disabled=['wtm.chat']`);
    await check(
        'With WTM Chat off, nothing is checked for that agent and Open says no',
        `(()=>{const r=${radios};const d=document.querySelector('[role=dialog]').innerText;return r.every(x=>x.disabled&&!x.checked)&&d.includes('WTM Chat is disabled in Settings')&&[...document.querySelectorAll('[role=dialog] button')].find(b=>b.textContent.trim()==='Open session').disabled})()`,
    );
    await evaluate(choose('codex'));
    await check(
        'With WTM Chat off, a native-capable agent offers Native CLI and plain only',
        `(()=>{const r=${radios};return !r[0].disabled&&r[1].disabled&&!r[2].disabled&&r[0].checked})()`,
    );
    await evaluate(`qa.plugins.state.registry.disabled=[]`);
    await click('Cancel');

    await evaluate(
        `qa.oldReviewListeners=[...(qa.listeners['prototype:changed']??[])];qa.reviews=[];qa.update({conversation:'new-conversation'})`,
    );
    // The old listeners are only "late" once the pane has moved to the new conversation.
    await settle(
        `document.body.innerText.includes('No design to review yet')`,
        'The conversation switch cleared the review',
    );
    await evaluate(
        `for(const id of qa.oldReviewListeners)qa.callbacks[id]?.({payload:{session:qa.current(),state:qa.review}})`,
    );
    await check(
        'Late events from a replaced conversation cannot restore its review',
        `document.body.innerText.includes('No design to review yet')&&!document.querySelector('.c-design-first')&&!document.querySelector('.c-native .c-banner')`,
    );
    await evaluate(`qa.publishReview()`);
    await call('Emulation.setDeviceMetricsOverride', {
        width: 390,
        height: 844,
        deviceScaleFactor: 1,
        mobile: false,
    });
    await check(
        'A small split keeps the terminal and a pane menu reachable',
        `document.documentElement.scrollWidth<=390&&document.querySelector('.c-terminal').getBoundingClientRect().height>250&&['More actions for this pane','Native session menu'].some(l=>(document.querySelector('button[aria-label="'+l+'"]')?.getBoundingClientRect().width??0)>0)`,
    );
    await call('Emulation.setDeviceMetricsOverride', {
        width: 300,
        height: 844,
        deviceScaleFactor: 1,
        mobile: false,
    });
    await check(
        'A folded header shows one menu, not two',
        `document.querySelector('button[aria-label="More actions for this pane"]').getBoundingClientRect().width>0&&document.querySelector('button[aria-label="Native session menu"]').getBoundingClientRect().width===0`,
    );
    await menu('More actions for this pane', 'Connection info…');
    await check(
        'A narrow pane folds its buttons and the native rows into one menu',
        `qa.menu.items.some(item=>item.text==='Split Right')&&qa.menu.items.some(item=>item.text==='Connection info…')&&!!document.querySelector('[role=dialog]')`,
    );
    await escape();
    await evaluate(`qa.sessions.panes[0].detached=true`);
    await check(
        'A pane holding a place with nothing behind it says Not running and offers Start',
        `${banner('Not running')}!==null&&!document.body.innerText.includes('Starting the installed CLI')&&document.querySelector('.xterm')!==null`,
    );
    const held = await evaluate(`qa.sessions.panes[0].providerSession`);
    const stopped = await evaluate(`qa.current()`);
    await clickIn('Not running', 'Start');
    await check(
        'Start resumes the pane’s conversation in place',
        `(()=>{const p=qa.sessions.panes[0];const call=qa.calls.filter(c=>c.command==='open_native_cli').at(-1);return !p.detached&&p.session!==${JSON.stringify(stopped)}&&call.args.integrated===true&&!!call.args.resume&&call.args.resume===${JSON.stringify(held)}&&${banner('Not running')}===null})()`,
    );

    // This fixture keeps the real session constructors and renders the real header Split button.
    // Only IPC is mocked, so a regression to open_terminal or a changed launch flag fails here.
    await call('Emulation.setDeviceMetricsOverride', {
        width: 1200,
        height: 900,
        deviceScaleFactor: 1,
        mobile: false,
    });
    await call('Page.navigate', {
        url: dev + '/scripts/fixtures/native/index.html?launch',
    });
    await settle(
        `window.qa?.launch===true&&typeof qa.resetLaunch==='function'&&!!qa.sessions&&!!${button('New agent session')}`,
        'The launch fixture mounted',
        30000,
    );
    const split = async () => {
        const control = `document.querySelector('button[aria-label="Split right"]')`;
        await settle(`(()=>{const b=${control};return !!b&&!b.disabled})()`, 'Split control');
        await evaluate(`${control}.click()`);
    };
    const sameLaunch = async (provider, experience) => {
        const command = experience === 'chat' ? 'open_agent_session' : 'open_native_cli';
        await check(
            `${provider} ${experience}: New and Split invoke the same backend request`,
            `(()=>{const calls=qa.calls.filter(c=>c.command===${JSON.stringify(command)});return calls.length===2&&JSON.stringify(calls[0].args)===JSON.stringify(calls[1].args)&&${experience === 'chat' ? `calls[0].args.agentId===${JSON.stringify(provider)}` : `calls[0].args.provider===${JSON.stringify(provider)}&&calls[0].args.integrated===${experience === 'native'}`}&&!qa.calls.some(c=>c.command==='open_terminal')})()`,
        );
        await check(
            `${provider} ${experience}: Split creates a fresh matching pane`,
            `(()=>{const [a,b]=qa.sessions.panes;return a.session!==b.session&&JSON.stringify(a.kind)===JSON.stringify(b.kind)&&!a.error&&!b.error&&${experience === 'chat' ? `qa.calls.find(c=>c.command==='open_agent_session').args.options.resume==null` : `qa.calls.find(c=>c.command==='open_native_cli').args.resume===null`}})()`,
        );
    };
    const experienceIndex = { native: 0, chat: 1, plain: 2 };
    const paneOpen = `qa.sessions.panes.length===1&&!!qa.sessions.panes[0].session`;
    for (const provider of ['claude', 'codex']) {
        await evaluate(
            `qa.resetLaunch();qa.sessions.openAgent('example','/tmp/example',${JSON.stringify(provider)})`,
        );
        await settle(paneOpen, 'The agent pane opened');
        await evaluate(`qa.plugins.state.registry.experience='wtm.chat'`);
        await split();
        await sameLaunch(provider, 'native');
        for (const experience of ['native', 'chat', 'plain']) {
            await evaluate(`qa.resetLaunch()`);
            await click('New agent session');
            await settle(`!!document.querySelector('#native-provider')`, 'The dialog opened');
            await evaluate(
                `(()=>{const select=document.querySelector('#native-provider');select.value=${JSON.stringify(provider)};select.dispatchEvent(new Event('change',{bubbles:true}))})()`,
            );
            const radio = `document.querySelectorAll('input[name=experience]')[${experienceIndex[experience]}]`;
            await settle(`!!${radio}&&!${radio}.disabled`, `${provider} offers ${experience}`);
            await evaluate(`${radio}.click()`);
            await click('Open session');
            await settle(paneOpen, 'The new session opened');
            await evaluate(
                `qa.plugins.state.registry.experience=${JSON.stringify(experience === 'native' ? 'wtm.chat' : 'wtm.native-cli')}`,
            );
            await split();
            await sameLaunch(provider, experience);
        }
    }
    await evaluate(
        `qa.resetLaunch();qa.plugins.state.registry.disabled=['wtm.chat'];qa.sessions.openAgent('example','/tmp/example','cursor')`,
    );
    await check(
        'With WTM Chat off, a chat-only agent is refused before a pane exists',
        `qa.sessions.panes.length===0&&qa.sessions.error==='WTM Chat is disabled in Settings → Plugins.'&&!qa.calls.some(c=>c.command==='open_agent_session')`,
    );
    await evaluate(`qa.plugins.state.registry.disabled=[]`);
    await evaluate(
        `(async()=>{qa.resetLaunch();await qa.sessions.openAgent('example','/tmp/example','claude');qa.source=qa.sessions.panes[0].id;await qa.sessions.openShell('example','/tmp/example','below');qa.other=qa.sessions.panes[1].id;})()`,
    );
    await split();
    await check(
        'Split anchors beside its source rather than the previously focused pane',
        `(()=>{const layout=qa.sessions.layouts['/tmp/example'];return layout.dir==='col'&&layout.a.dir==='row'&&layout.a.a.paneId===qa.source&&layout.b.paneId===qa.other})()`,
    );
    await evaluate(
        `(async()=>{qa.resetLaunch();await qa.sessions.openPluginTerminal('example','/tmp/example','example.tools','terminal','Example terminal');await qa.sessions.splitPane(qa.sessions.panes[0].id)})()`,
    );
    await check(
        'Plugin terminal splits preserve the declared profile',
        `(()=>{const calls=qa.calls.filter(c=>c.command==='open_plugin_terminal');return calls.length===2&&JSON.stringify(calls[0].args)===JSON.stringify(calls[1].args)&&!qa.calls.some(c=>c.command==='open_terminal')})()`,
    );
    if (errors.length) throw Error(JSON.stringify(errors));
    console.log(
        'PASS: native header/menu fold, full terminal, hidden-review retention, exact approval, Delivered state, busy/stale/preview gating with reasons, withdraw/not-delivered/check without replay, earlier receipts, one Reconnect confirm through the real restart that keeps its receipts, explicit fallback, experience fallback, Not running/Start, responsive layout, CLI Escape ownership, shared New/Split launch and placement',
    );
} finally {
    await fetch('http://127.0.0.1:' + port + '/json/close/' + page.id);
    ws.close();
}

/** Actual native-session Svelte regression with explicitly mocked IPC. No provider requests.
 * Start an isolated Chrome with a remote debugging port and the Vite dev server, then:
 * WTM_CDP_PORT=9393 WTM_DEV_URL=http://localhost:5392 node scripts/check-native-ui.mjs
 * Optional WTM_SHOTS writes screenshots. The probe opens and closes only its own browser tab.
 */
import fs from 'node:fs/promises';
const port = process.env.WTM_CDP_PORT;
if (!port)
    throw Error(
        'Set WTM_CDP_PORT to an isolated Chrome remote-debugging port. Start the project dev server first.',
    );
const dev = process.env.WTM_DEV_URL ?? 'http://localhost:5392';
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
const wait = (n = 220) => new Promise((r) => setTimeout(r, n));
const click = async (text) => {
    await evaluate(
        `(()=>{const root=[...document.querySelectorAll('[role=dialog]')].at(-1)||document;const b=[...root.querySelectorAll('button')].find(b=>b.textContent.trim()===${JSON.stringify(text)});if(!b||b.disabled)throw Error('Missing or disabled '+${JSON.stringify(text)});b.click()})()`,
    );
    await wait();
};
const check = async (name, expression) => {
    const result = await evaluate(expression);
    console.log(name, result);
    if (result !== true) throw Error(name);
};
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

try {
    await call('Runtime.enable');
    await call('Page.enable');
    await call('Emulation.setDeviceMetricsOverride', {
        width: 1200,
        height: 900,
        deviceScaleFactor: 1,
        mobile: false,
    });
    await call('Page.navigate', { url: dev + '/scripts/fixtures/native/index.html' });
    await wait(2500);
    await check(
        'The native pane opens to the terminal without activity or status chrome',
        `document.querySelector('.xterm')!==null&&document.querySelector('.c-native__aside').hidden&&!document.querySelector('.c-native__notice')&&!document.body.innerText.includes('Activity')&&!document.body.innerText.includes('Session details')&&!document.body.innerText.includes('Reported by the CLI')`,
    );
    await check(
        'The terminal uses the full available width and height',
        `document.querySelector('.c-terminal').getBoundingClientRect().width>1100&&document.querySelector('.c-terminal').getBoundingClientRect().height>650`,
    );
    await check(
        'Open Design belongs to the existing pane header',
        `document.querySelectorAll('.c-pane__head').length===1&&[...document.querySelector('.c-pane__head').querySelectorAll('button')].some(b=>b.textContent.trim()==='Open Design')`,
    );
    await shot('native-dark');
    const menu = async (choice) => {
        await evaluate(
            `qa.menuChoice=${JSON.stringify(choice)};document.querySelector('button[aria-label="More actions for this pane"]').click()`,
        );
        await wait();
    };
    await menu('Connection info…');
    await check(
        'The existing native menu opens connection information',
        `document.querySelector('[role=dialog]').innerText.includes('Connection info')&&document.querySelector('[role=dialog]').innerText.includes('Native CLI')&&qa.menu.items.some(item=>item.text==='Split Right')`,
    );
    await call('Input.dispatchKeyEvent', {
        type: 'keyDown',
        key: 'Escape',
        code: 'Escape',
        windowsVirtualKeyCode: 27,
        nativeVirtualKeyCode: 27,
    });
    await call('Input.dispatchKeyEvent', {
        type: 'keyUp',
        key: 'Escape',
        code: 'Escape',
        windowsVirtualKeyCode: 27,
        nativeVirtualKeyCode: 27,
    });
    await wait();
    await check(
        'Escape closes connection info',
        `!document.querySelector('[role=dialog]')`,
    );
    await click('Open Design');
    await check(
        'Open Design has an empty state before a review exists',
        `document.body.innerText.includes('No design to review yet')&&!document.querySelector('.c-native__aside').hidden`,
    );
    await click('Hide');
    const replays = await evaluate(
        `qa.calls.filter(c=>c.command==='terminal_replay').length`,
    );
    await evaluate(`qa.publishReview()`);
    await wait();
    await check(
        'A published review opens its previously hidden region',
        `!document.querySelector('.c-native__aside').hidden&&document.body.innerText.includes('Example navigation')`,
    );
    await shot('native-review-dark');
    await evaluate(
        `(()=>{const input=document.querySelector('.c-design-first__feedback textarea');input.value='Keep this feedback';input.dispatchEvent(new Event('input',{bubbles:true}))})()`,
    );
    await click('Hide');
    await evaluate(`qa.update({status:'working'})`);
    await wait();
    await check(
        'Status updates do not reopen a hidden review or duplicate the terminal',
        `document.querySelector('.c-native__aside').hidden&&qa.calls.filter(c=>c.command==='terminal_replay').length===${replays}`,
    );
    await click('Open Design');
    await check(
        'Hiding the review preserves its feedback',
        `document.querySelector('.c-design-first__feedback textarea').value==='Keep this feedback'`,
    );
    await evaluate(
        `(()=>{const input=document.querySelector('.c-design-first__feedback textarea');input.value='';input.dispatchEvent(new Event('input',{bubbles:true}))})()`,
    );
    await check(
        'A busy native turn cannot accept approval',
        `[...document.querySelectorAll('button')].find(b=>b.textContent.trim()==='Implement this design').disabled`,
    );
    await evaluate(`qa.update({status:'idle'});qa.publishReview({stale:true})`);
    await wait();
    await check(
        'A stale review cannot accept approval',
        `[...document.querySelectorAll('button')].find(b=>b.textContent.trim()==='Implement this design').disabled`,
    );
    await evaluate(`qa.publishReview({stale:false})`);
    await wait();
    await evaluate(`document.querySelector('.xterm-helper-textarea').focus()`);
    await call('Input.dispatchKeyEvent', {
        type: 'keyDown',
        key: 'Escape',
        code: 'Escape',
        windowsVirtualKeyCode: 27,
        nativeVirtualKeyCode: 27,
    });
    await call('Input.dispatchKeyEvent', {
        type: 'keyUp',
        key: 'Escape',
        code: 'Escape',
        windowsVirtualKeyCode: 27,
        nativeVirtualKeyCode: 27,
    });
    await wait();
    await check(
        'Escape in the native terminal leaves the review open',
        `!document.querySelector('.c-native__aside').hidden&&qa.calls.some(c=>c.command==='pty_write'&&c.args.dataBase64==='Gw==')`,
    );
    await evaluate(`document.querySelector('.c-native__aside button').focus()`);
    await call('Input.dispatchKeyEvent', {
        type: 'keyDown',
        key: 'Escape',
        code: 'Escape',
        windowsVirtualKeyCode: 27,
        nativeVirtualKeyCode: 27,
    });
    await call('Input.dispatchKeyEvent', {
        type: 'keyUp',
        key: 'Escape',
        code: 'Escape',
        windowsVirtualKeyCode: 27,
        nativeVirtualKeyCode: 27,
    });
    await wait();
    await check(
        'Escape in the review returns focus to its header control',
        `document.querySelector('.c-native__aside').hidden&&document.activeElement.textContent.trim()==='Open Design'`,
    );
    await click('Open Design');
    await click('Implement this design');
    await check(
        'The exact review is dispatched once',
        `qa.calls.filter(c=>c.command==='implement_prototype').length===1&&qa.calls.find(c=>c.command==='implement_prototype').args.reviewId==='review'`,
    );
    await check(
        'A delivered approval cannot be clicked again',
        `[...document.querySelectorAll('button')].find(b=>b.textContent.trim()==='Delivered to this conversation').disabled`,
    );
    await evaluate(
        `qa.update({provider:'claude',capabilities:{send:false,stop:false,plans:false,usage:false,permissionOwner:'native_cli'},deliveries:[]})`,
    );
    await wait();
    await click('Implement this design');
    await click('Hide');
    await check(
        'Claude continuation remains visible when the review is hidden',
        `document.querySelector('.c-native__aside').hidden&&document.querySelector('.c-native__notice').innerText.includes('Send your next message')&&qa.native.states['qa-native'].deliveries[0].status==='pending'`,
    );
    await evaluate(
        `qa.update({status:'disconnected',deliveries:qa.native.states['qa-native'].deliveries.map(r=>({...r,status:'unknown'}))})`,
    );
    await wait();
    const sent = await evaluate(
        `qa.calls.filter(c=>c.command==='implement_prototype').length`,
    );
    await click('Reconnect');
    await click('Check delivery');
    await check(
        'Reconnect and receipt refresh never replay uncertain delivery',
        `qa.calls.filter(c=>c.command==='implement_prototype').length===${sent}&&qa.native.states['qa-native'].deliveries[0].status==='unknown'`,
    );
    await check(
        'Checking delivery completes without an IPC error',
        `!document.querySelector('.c-native__notice.c-status--danger')`,
    );
    await shot('native-pending-dark');
    await evaluate(
        `qa.update({status:'unavailable',error:'Example adapter failed to connect'})`,
    );
    await wait();
    await click('Review connection');
    await click('Open plain terminal');
    await check(
        'Plain fallback opens a separate session and retains the original',
        `qa.calls.some(c=>c.command==='open_native_cli'&&c.args.integrated===false)&&qa.sessions.panes[0].session==='qa-native'&&qa.sessions.panes.length===2`,
    );
    await evaluate(
        `qa.update({status:'idle',error:null,deliveries:[]});qa.publishReview({prototype:{...qa.review.prototype,implementedReview:'review'}})`,
    );
    await wait();
    await check(
        'A completed review clears its reminder',
        `!document.querySelector('.c-native__notice')`,
    );
    await call('Emulation.setDeviceMetricsOverride', {
        width: 760,
        height: 900,
        deviceScaleFactor: 1,
        mobile: false,
    });
    await evaluate(`document.documentElement.dataset.theme='light';qa.theme.applied++`);
    await wait();
    await shot('native-light');
    await click('Open Design');
    await evaluate(
        `qa.publishReview({prototype:{...qa.review.prototype,implementedReview:null,review:{...qa.review.prototype.review,id:'review-2'}}})`,
    );
    await wait();
    await check(
        'A new immutable revision is revealed',
        `!document.querySelector('.c-native__aside').hidden`,
    );
    await shot('native-review-light');
    await check(
        'The narrow review and terminal fit without overflow',
        `document.documentElement.scrollWidth<=760&&document.querySelector('.c-terminal').getBoundingClientRect().height>400`,
    );
    await click('New agent session');
    await check(
        'All three new-session experiences remain explicit',
        `document.querySelectorAll('input[name=experience]').length===3`,
    );
    await click('Cancel');

    await evaluate(
        `qa.oldReviewListeners=[...(qa.listeners['prototype:changed']??[])];qa.reviews=[];qa.update({conversation:'new-conversation'})`,
    );
    await wait();
    await evaluate(
        `for(const id of qa.oldReviewListeners)qa.callbacks[id]?.({payload:{session:'qa-native',state:qa.review}})`,
    );
    await wait();
    await check(
        'Late events from a replaced conversation cannot restore its review',
        `document.body.innerText.includes('No design to review yet')&&!document.querySelector('.c-design-first')&&!document.querySelector('.c-native__notice')`,
    );
    await evaluate(`qa.publishReview()`);
    await wait();
    await call('Emulation.setDeviceMetricsOverride', {
        width: 390,
        height: 844,
        deviceScaleFactor: 1,
        mobile: false,
    });
    await wait();
    await check(
        'A small split keeps the terminal and pane menu reachable',
        `document.documentElement.scrollWidth<=390&&document.querySelector('.c-terminal').getBoundingClientRect().height>250&&document.querySelector('button[aria-label="More actions for this pane"]').getBoundingClientRect().width>0`,
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
    await wait(2500);
    const split = async () => {
        await evaluate(
            `(()=>{const b=document.querySelector('button[aria-label="Split right"]');if(!b||b.disabled)throw Error('Missing Split control');b.click()})()`,
        );
        await wait();
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
    for (const provider of ['claude', 'codex']) {
        await evaluate(
            `qa.resetLaunch();qa.sessions.openAgent('example','/tmp/example',${JSON.stringify(provider)})`,
        );
        await wait();
        await evaluate(`qa.plugins.state.registry.experience='wtm.chat'`);
        await split();
        await sameLaunch(provider, 'native');
        for (const experience of ['native', 'chat', 'plain']) {
            await evaluate(`qa.resetLaunch()`);
            await click('New agent session');
            await evaluate(
                `(()=>{const select=document.querySelector('#native-provider');select.value=${JSON.stringify(provider)};select.dispatchEvent(new Event('change',{bubbles:true}));document.querySelector('input[value=${experience}]').click()})()`,
            );
            await click('Open session');
            await evaluate(
                `qa.plugins.state.registry.experience=${JSON.stringify(experience === 'native' ? 'wtm.chat' : 'wtm.native-cli')}`,
            );
            await split();
            await sameLaunch(provider, experience);
        }
    }
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
        'PASS: native header/menu, full terminal, hidden-review retention, exact approval, busy/stale gating, no replay, explicit fallback, responsive layout, CLI Escape ownership, shared New/Split launch and placement',
    );
} finally {
    await fetch('http://127.0.0.1:' + port + '/json/close/' + page.id);
    ws.close();
}

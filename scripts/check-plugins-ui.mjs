/** Actual Svelte contribution/editor regression with explicitly mocked IPC. No provider requests.
 * Start an isolated Chrome with a remote debugging port and the Vite dev server, then:
 * WTM_CDP_PORT=9393 WTM_DEV_URL=http://localhost:5392 node scripts/check-plugins-ui.mjs
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
    await call('Runtime.discardConsoleEntries');
    await call('Runtime.enable');
    errors.length = 0;
    await call('Page.enable');
    await call('Emulation.setDeviceMetricsOverride', {
        width: 1200,
        height: 900,
        deviceScaleFactor: 1,
        mobile: false,
    });
    await call('Page.navigate', { url: dev + '/scripts/fixtures/plugins/index.html' });
    await wait(1600);
    await check(
        'before hooks transform validated arguments and recheck authority',
        `(async()=>{
 const manifest=qa.plugins.state.registry.installed['example.workbench-toolkit'].current.manifest;
 const originalHooks=manifest.extensions.hooks;
 let applied=false;
 try{
  manifest.extensions.hooks=[{id:'rename-hook',title:'Rename hook',operation:'ui.workspace.rename',phase:'before'}];
  qa.hookResult={arguments:{name:'Transformed'}};
  const result=await qa.extensions.hooks('ui.workspace.rename',{name:'Original'},async(args)=>{applied=qa.calls.some(c=>c.command==='extension_validate'&&c.args.arguments.name==='Transformed');return args.name});
  if(result!=='Transformed'||!applied)return false;
  qa.hookResult={arguments:{name:123}};
  let refused=false;try{await qa.extensions.hooks('ui.workspace.rename',{name:'Original'},async()=>{throw Error('Invalid arguments were dispatched')})}catch(e){refused=e.message.includes('published contract')}
  return refused;
 }finally{manifest.extensions.hooks=originalHooks;delete qa.hookResult}
})()`,
    );
    await check(
        'wire schemas reject malformed and protected calls',
        `(async()=>{
 const {validateExtensionArguments:validate}=await import('/src/lib/extension-schema.ts');
 validate('ui.workspace.rename',{name:'New name'});
 validate('ui.panels.move',{paneId:'one',target:{kind:'pane',paneId:'two',side:'right'}});
 validate('list_projects',{});
 for(const [operation,args] of [['ui.workspace.rename',{name:12}],['ui.composer.write',{text:'draft'}],['ui.panels.open',{kind:'invented'}],['ui.workspace.rename',{name:'x',unexpected:true}],['answer_approval',{}]]) {
  let refused=false;try{validate(operation,args)}catch{refused=true}if(!refused)return false;
 }
 return true;
})()`,
    );
    await check(
        'repeated and paged echoes retain their own provider identities',
        `(async()=>{
 const {messageIdentities}=await import('/src/lib/message-identities.ts');
 const first={kind:'user_echo',text:'Same'},second={kind:'user_echo',text:'Same'};
 const events=[first,{kind:'history_boundary',complete:true},{kind:'message_identity',message:'m1',turn:'t1',text:'Same',attachments:false},{kind:'turn_finished'},...Array.from({length:300},()=>({kind:'message_delta',text:'x'})),second,{kind:'message_identity',message:'m2',turn:'t2',text:'Same',attachments:false},{kind:'turn_finished'}];
 const ids=messageIdentities(events);
 const ambiguous={kind:'user_echo',text:'Same'},other={kind:'user_echo',text:'Same'};
 return ids.get(first)==='m1'&&ids.get(second)==='m2'&&messageIdentities([ambiguous,other,{kind:'message_identity',message:'m3',turn:'t3',text:'Same'}]).size===0;
})()`,
    );
    await evaluate(`document.querySelector('.c-plugins__item').click()`);
    await wait();
    await check(
        'reviewed API and native runtime are disclosed',
        `document.body.innerText.includes('Plugin API 2')&&document.body.innerText.includes('Runs executable code as your user')`,
    );
    await shot('wtm-extensions-implemented-dark');
    await click('Chat fixture');
    await check(
        'chat styles target the existing markdown link',
        `document.querySelector('.c-markdown a').style.textDecoration==='underline'`,
    );
    await check(
        'content and settings are inserted',
        `document.body.innerText.includes('Workspace: /tmp/qa-workspace')&&document.querySelector('[id="extension-setting-example.workbench-toolkit:workspace-label"]')!==null`,
    );
    await evaluate(
        `qa.extensions.configure('example.workbench-toolkit',{kind:'enabled',id:'readable-links',enabled:false})`,
    );
    await wait();
    await check(
        'disabling contribution restores host link styling',
        `document.querySelector('.c-markdown a').style.textDecoration===''`,
    );
    await evaluate(
        `qa.extensions.configure('example.workbench-toolkit',{kind:'enabled',id:'readable-links',enabled:true})`,
    );
    await wait();
    await evaluate(
        `qa.extensions.configure('example.workbench-toolkit',{kind:'scope',worktrees:['/another-workspace']})`,
    );
    await wait();
    await check(
        'scope removes content and styles together',
        `!document.body.innerText.includes('Workspace: /tmp/qa-workspace')&&document.querySelector('.c-markdown a').style.textDecoration===''`,
    );
    await evaluate(
        `qa.extensions.configure('example.workbench-toolkit',{kind:'scope',worktrees:[]})`,
    );
    await wait();
    await evaluate(
        `qa.extensions.configure('example.workbench-toolkit',{kind:'setting',id:'workspace-label',worktree:'',value:'Custom label'})`,
    );
    await wait();
    await check(
        'namespaced setting is applied',
        `document.querySelector('[id="extension-setting-example.workbench-toolkit:workspace-label"]').value==='Custom label'`,
    );
    await evaluate(`qa.chatEdit.open('qa-pane','m1')`);
    await wait();
    await check(
        'edit explicitly retains original history',
        `document.body.innerText.includes('original conversation')&&document.querySelector('#chat-edit-message').value==='Use links in this reply.'`,
    );
    await evaluate(
        `document.querySelector('#chat-edit-message').value='Use clearer links.';document.querySelector('#chat-edit-message').dispatchEvent(new Event('input',{bubbles:true}))`,
    );
    await wait();
    await shot('wtm-extensions-edit-dark');
    await click('Create branch and send');
    await wait();
    await check(
        'one reviewed request opens a separate branch',
        `qa.calls.filter(c=>c.command==='chat_edit').length===1&&qa.calls.some(c=>c.command==='show-branch')&&qa.calls.find(c=>c.command==='chat_edit').args.edit.text==='Use clearer links.'`,
    );
    await click('Done');
    await evaluate(`qa.busy=true;qa.chatEdit.open('qa-pane','m1')`);
    await wait();
    await check(
        'busy edit cannot submit',
        `[...document.querySelectorAll('button')].some(b=>b.textContent.trim()==='Stop turn')&&![...document.querySelectorAll('button')].some(b=>b.textContent.trim()==='Create branch and send')`,
    );
    await click('Cancel');
    await evaluate(`qa.busy=true;qa.chatEdit.open('qa-pane','m1')`);
    await wait();
    await click('Stop turn');
    await check(
        'busy edit uses the existing stop operation before continuing',
        `qa.calls.filter(c=>c.command==='interrupt_turn').length===1&&![...document.querySelectorAll('button')].find(b=>b.textContent.trim()==='Create branch and send').disabled`,
    );
    await click('Cancel');
    await evaluate(
        `qa.unsupported=true;qa.draft='Existing draft';qa.chatEdit.open('qa-pane','m1')`,
    );
    await wait();
    await click('Copy to composer');
    await check(
        'unsupported editing keeps the draft and copies without sending',
        `qa.draft===['Existing draft','','Use links in this reply.'].join(String.fromCharCode(10))&&qa.calls.filter(c=>c.command==='chat_edit').length===1&&!qa.chatEdit.opened`,
    );
    await evaluate(`qa.unsupported=false`);
    await evaluate(`qa.busy=false;qa.stale=true;qa.chatEdit.open('qa-pane','m1')`);
    await wait();
    await click('Create branch and send');
    await check(
        'stale edit reports failure without another branch',
        `document.body.innerText.includes('Conversation changed')&&qa.calls.filter(c=>c.command==='show-branch').length===1`,
    );
    await click('Cancel');
    await evaluate(`qa.stale=false;qa.chatEdit.open('qa-pane','m1')`);
    await wait();
    await call('Emulation.setDeviceMetricsOverride', {
        width: 760,
        height: 900,
        deviceScaleFactor: 1,
        mobile: false,
    });
    await evaluate(`document.documentElement.dataset.theme='light'`);
    await wait();
    await shot('wtm-extensions-edit-light');
    await check(
        'editor fits the narrow viewport',
        `(()=>{const d=document.querySelector('[role=dialog]'),r=d.getBoundingClientRect();return r.left>=0&&r.right<=innerWidth&&r.bottom<=innerHeight&&d.scrollWidth<=d.clientWidth})()`,
    );
    await call('Input.dispatchKeyEvent', {
        type: 'keyDown',
        key: 'Escape',
        code: 'Escape',
        windowsVirtualKeyCode: 27,
    });
    await wait();
    await check(
        'Escape closes the editor',
        `document.querySelector('[role=dialog]')===null`,
    );
    await click('Plugins');
    await click('Develop a plugin');
    await evaluate(
        `const i=document.querySelector('input[aria-label="Search public contracts"]');i.value='chat.';i.dispatchEvent(new Event('input',{bubbles:true}))`,
    );
    await wait();
    await check(
        'inventory can find chat contracts',
        `document.body.innerText.includes('ui.chat.edit')`,
    );
    await shot('wtm-extensions-inventory-light');
    console.log('errors', errors);
    if (errors.length) throw Error('Unexpected runtime exceptions');
} finally {
    await call('Page.close').catch(() => {});
    ws.close();
}

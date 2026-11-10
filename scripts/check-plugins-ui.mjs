/** Actual Svelte contribution/editor regression with explicitly mocked IPC. No provider requests.
 * Start the Vite dev server (`bun x --bun vite`, which serves on 5173) and an isolated headless
 * Chrome with a remote debugging port and a fresh profile, then:
 * WTM_CDP_PORT=9393 [WTM_DEV_URL=http://localhost:5173] bun scripts/check-plugins-ui.mjs
 * Optional WTM_SHOTS writes screenshots. The probe opens and closes only its own browser tab.
 *
 * Nothing here sleeps for a fixed time. Every step waits for the state it needs, up to a few
 * seconds, so a slow machine is slower rather than wrong and a fast one does not wait for nothing.
 * Focus is emulated, because a headless page in the background moves `activeElement` without
 * delivering focus or key events, which is what Escape and focus assertions depend on.
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
const noDialog = `document.querySelector('[role=dialog]')===null`;
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
    await call('Emulation.setFocusEmulationEnabled', { enabled: true });
    await call('Emulation.setDeviceMetricsOverride', {
        width: 1200,
        height: 900,
        deviceScaleFactor: 1,
        mobile: false,
    });
    await call('Page.navigate', { url: dev + '/scripts/fixtures/plugins/index.html' });
    await settle(
        `!!window.qa?.extensions&&document.querySelector('.c-plugins__item')!==null`,
        'The fixture mounted',
        30000,
    );
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
 const {MessageIdentities}=await import('/src/lib/message-identities.ts');
 const first={kind:'user_echo',text:'Same'},second={kind:'user_echo',text:'Same'};
 const events=[first,{kind:'history_boundary',complete:true},{kind:'message_identity',message:'m1',turn:'t1',text:'Same',attachments:false},{kind:'turn_finished'},...Array.from({length:300},()=>({kind:'message_delta',text:'x'})),second,{kind:'message_identity',message:'m2',turn:'t2',text:'Same',attachments:false},{kind:'turn_finished'}];
 const ids=new MessageIdentities();ids.update(events);
 const ambiguous={kind:'user_echo',text:'Same'},other={kind:'user_echo',text:'Same'};
 const unsure=new MessageIdentities();unsure.update([ambiguous,other,{kind:'message_identity',message:'m3',turn:'t3',text:'Same'}]);
 return ids.get(first)==='m1'&&ids.get(second)==='m2'&&unsure.get(ambiguous)===undefined&&unsure.get(other)===undefined;
})()`,
    );
    await check(
        'identities fold incrementally, through a moved patch, a trim and a replaced log',
        `(async()=>{
 const {MessageIdentities}=await import('/src/lib/message-identities.ts');
 const ids=new MessageIdentities();
 const echo={kind:'user_echo',text:'One'},patch={kind:'patch',id:'p',diff:'a'};
 const events=[{kind:'message_delta',text:'x'},echo,patch];ids.update(events);
 events.splice(2,1);events.push({kind:'message_delta',text:'y'},{kind:'patch',id:'p',diff:'b'},{kind:'message_identity',message:'m1',turn:'t1',text:'One'});ids.update(events);
 if(ids.get(echo)!=='m1')return false;
 events.splice(0,1);const later={kind:'user_echo',text:'Two'};events.push({kind:'turn_finished'},later,{kind:'message_identity',message:'m2',turn:'t2',text:'Two'});ids.update(events);
 if(ids.get(later)!=='m2')return false;
 const fresh={kind:'user_echo',text:'Three'};ids.update([fresh,{kind:'message_identity',message:'m3',turn:'t3',text:'Three'}]);
 return ids.get(fresh)==='m3';
})()`,
    );
    await check(
        'a replacement waits for an explicit choice, and its result must match the contract',
        `(async()=>{
 const manifest=qa.plugins.state.registry.installed['example.workbench-toolkit'].current.manifest;
 const originalActions=manifest.extensions.actions;
 let builtin=0;
 const next=async(args)=>{builtin++;return {id:'/tmp/qa-workspace',name:args.name,path:'/tmp/qa-workspace'}};
 try{
  manifest.extensions.actions=[...originalActions,{id:'rename-elsewhere',title:'Rename elsewhere',target:'workspace.actions',operation:'plugin.rename',arguments:{},replaces:'ui.workspace.rename'}];
  qa.hookResult={id:'/tmp/qa-workspace',name:'Replaced',path:'/tmp/qa-workspace'};
  const unchosen=await qa.extensions.hooks('ui.workspace.rename',{name:'Original'},next);
  if(builtin!==1||unchosen.name!=='Original')return false;
  qa.plugins.state.registry.extension_choices['ui.workspace.rename']='example.workbench-toolkit:rename-elsewhere';
  const chosen=await qa.extensions.hooks('ui.workspace.rename',{name:'Original'},next);
  if(builtin!==1||chosen.name!=='Replaced')return false;
  qa.hookResult={unexpected:true};
  let refused=false;try{await qa.extensions.hooks('ui.workspace.rename',{name:'Original'},next)}catch(e){refused=e.message.includes('published contract')}
  return refused&&builtin===1;
 }finally{manifest.extensions.actions=originalActions;delete qa.plugins.state.registry.extension_choices['ui.workspace.rename'];delete qa.hookResult}
})()`,
    );
    await evaluate(`document.querySelector('.c-plugins__item').click()`);
    await check(
        'reviewed API and native runtime are disclosed',
        `document.body.innerText.includes('Plugin API 2')&&document.body.innerText.includes('acts as native code')&&!document.body.innerText.includes('Isolated local UI')`,
    );
    await check(
        'declared host access is listed with the catalog reason',
        `(()=>{const t=document.body.textContent;return t.includes('ui.workspace.rename')&&t.includes('Asks wtm to')&&t.includes('display label, never its path')})()`,
    );
    await check(
        'an argv is quoted the way a reader can split it',
        `(async()=>{const {argvText}=await import('/src/lib/plugins.ts');return argvText(['python3','runtime.py'])==='python3 runtime.py'&&argvText(['run','a b'])==="run 'a b'"&&argvText(["it's"])===["'it'",String.fromCharCode(92),"''s'"].join('')})()`,
    );
    await check(
        'a move rearranges one group and keeps every other saved position',
        `(async()=>{const {mergeOrder}=await import('/src/lib/plugins.ts');const out=mergeOrder(['k1','a','k2','y','x','k3'],['a','x','y','b']);return JSON.stringify(out)===JSON.stringify(['k1','a','k2','x','y','k3','b'])})()`,
    );
    await evaluate(`qa.refuseEnable=true`);
    await evaluate(
        `[...document.querySelectorAll('.c-plugins__detail label')].find(l=>l.textContent.trim()==='Enabled').querySelector('input').click()`,
    );
    await check(
        'a refused toggle shows its real state and the reason once',
        `(()=>{const box=[...document.querySelectorAll('.c-plugins__detail label')].find(l=>l.textContent.trim()==='Enabled').querySelector('input');const alerts=[...document.querySelectorAll('[role=alert]')].filter(a=>a.textContent.includes('sessions first'));return box.checked===true&&alerts.length===1})()`,
    );
    await evaluate(`qa.refuseEnable=false`);
    await click('Reload & review…');
    await click('Review plugin');
    await check(
        'an update with nothing new says so and cannot be installed',
        `(()=>{const d=[...document.querySelectorAll('[role=dialog]')].at(-1);const install=[...d.querySelectorAll('button')].find(b=>b.textContent.trim()==='Install & enable');return d.getAttribute('aria-label')==='Review update'&&d.innerText.includes('No changes since the installed version')&&install.disabled&&document.activeElement?.tagName==='H3'})()`,
    );
    await click('Cancel');
    await settle(noDialog, 'The review closed');
    await shot('wtm-extensions-implemented-dark');
    await click('Chat fixture');
    await check(
        'chat styles target the existing markdown link',
        `document.querySelector('.c-markdown a').style.textDecoration==='underline'`,
    );
    await check(
        'contributions render through a built-in plugin slot',
        `document.querySelector('[aria-label="Through a plugin slot"] [data-wtm-contribution="example.workbench-toolkit:workspace-note"]')!==null`,
    );
    await check(
        'content and settings are inserted',
        `document.body.innerText.includes('Workspace: /tmp/qa-workspace')&&document.querySelector('[id="extension-setting-example.workbench-toolkit:workspace-label"]')!==null`,
    );
    await evaluate(
        `qa.extensions.configure('example.workbench-toolkit',{kind:'enabled',id:'readable-links',enabled:false})`,
    );
    await check(
        'disabling contribution restores host link styling',
        `document.querySelector('.c-markdown a').style.textDecoration===''`,
    );
    await evaluate(
        `qa.extensions.configure('example.workbench-toolkit',{kind:'enabled',id:'readable-links',enabled:true})`,
    );
    // Back on first, or the scope check below would pass on a link that was never restyled.
    await settle(
        `document.querySelector('.c-markdown a').style.textDecoration==='underline'`,
        'Re-enabling the style restyles the link',
    );
    await evaluate(
        `qa.extensions.configure('example.workbench-toolkit',{kind:'scope',worktrees:['/another-workspace']})`,
    );
    await check(
        'scope removes content and styles together',
        `!document.body.innerText.includes('Workspace: /tmp/qa-workspace')&&document.querySelector('.c-markdown a').style.textDecoration===''`,
    );
    await evaluate(
        `qa.extensions.configure('example.workbench-toolkit',{kind:'scope',worktrees:[]})`,
    );
    await settle(
        `document.body.innerText.includes('Workspace: /tmp/qa-workspace')`,
        'Clearing the scope brings the content back',
    );
    await evaluate(
        `qa.extensions.configure('example.workbench-toolkit',{kind:'setting',id:'workspace-label',worktree:'',value:'Custom label'})`,
    );
    await check(
        'namespaced setting is applied',
        `document.querySelector('[id="extension-setting-example.workbench-toolkit:workspace-label"]').value==='Custom label'`,
    );
    await evaluate(`qa.chatEdit.open('qa-pane','m1')`);
    await check(
        'edit explicitly retains original history',
        `document.body.innerText.includes('original conversation')&&document.querySelector('#chat-edit-message').value==='Use links in this reply.'`,
    );
    await evaluate(
        `document.querySelector('#chat-edit-message').value='Use clearer links.';document.querySelector('#chat-edit-message').dispatchEvent(new Event('input',{bubbles:true}))`,
    );
    await settle(
        `document.querySelector('#chat-edit-message').value==='Use clearer links.'`,
        'The edited text is in the editor',
    );
    await shot('wtm-extensions-edit-dark');
    await click('Send in a new conversation');
    await check(
        'one reviewed request opens a separate conversation',
        `qa.calls.filter(c=>c.command==='chat_edit').length===1&&qa.calls.some(c=>c.command==='show-branch')&&qa.calls.find(c=>c.command==='chat_edit').args.edit.text==='Use clearer links.'`,
    );
    await click('Done');
    await evaluate(`qa.busy=true;qa.chatEdit.open('qa-pane','m1')`);
    await check(
        'busy edit cannot submit',
        `[...document.querySelectorAll('button')].some(b=>b.textContent.trim()==='Stop turn')&&![...document.querySelectorAll('button')].some(b=>b.textContent.trim()==='Send in a new conversation')`,
    );
    await click('Cancel');
    await evaluate(`qa.busy=true;qa.chatEdit.open('qa-pane','m1')`);
    await click('Stop turn');
    await check(
        'busy edit uses the existing stop operation before continuing',
        `qa.calls.filter(c=>c.command==='interrupt_turn').length===1&&![...document.querySelectorAll('button')].find(b=>b.textContent.trim()==='Send in a new conversation').disabled`,
    );
    await click('Cancel');
    await evaluate(
        `qa.unsupported=true;qa.draft='Existing draft';qa.chatEdit.open('qa-pane','m1')`,
    );
    await click('Copy to composer');
    await check(
        'unsupported editing keeps the draft and copies without sending',
        `qa.draft===['Existing draft','','Use links in this reply.'].join(String.fromCharCode(10))&&qa.calls.filter(c=>c.command==='chat_edit').length===1&&!qa.chatEdit.opened`,
    );
    await evaluate(`qa.unsupported=false`);
    await evaluate(`qa.busy=false;qa.stale=true;qa.chatEdit.open('qa-pane','m1')`);
    await click('Send in a new conversation');
    await check(
        'stale edit reports failure without another conversation',
        `document.body.innerText.includes('Conversation changed')&&qa.calls.filter(c=>c.command==='show-branch').length===1`,
    );
    await check(
        'a failed send offers Retry with the same request and locks the text',
        `(()=>{const retry=[...document.querySelectorAll('button')].find(b=>b.textContent.trim()==='Retry');return !!retry&&!retry.disabled&&document.querySelector('#chat-edit-message').disabled})()`,
    );
    await click('Retry');
    await check(
        'Retry sends the same request id again',
        `(()=>{const edits=qa.calls.filter(c=>c.command==='chat_edit');const [a,b]=edits.slice(-2);return edits.length>=2&&a.args.edit.id===b.args.edit.id})()`,
    );
    await click('Cancel');
    await evaluate(`qa.stale=false;qa.chatEdit.open('qa-pane','m1')`);
    await call('Emulation.setDeviceMetricsOverride', {
        width: 760,
        height: 900,
        deviceScaleFactor: 1,
        mobile: false,
    });
    await evaluate(`document.documentElement.dataset.theme='light'`);
    await settle(`document.querySelector('[role=dialog]')!==null`, 'The editor is open');
    await shot('wtm-extensions-edit-light');
    await check(
        'editor fits the narrow viewport',
        `(()=>{const d=document.querySelector('[role=dialog]'),r=d.getBoundingClientRect();return r.left>=0&&r.right<=innerWidth&&r.bottom<=innerHeight&&d.scrollWidth<=d.clientWidth})()`,
    );
    // Pressed only once the editor is open and closable, or a missing dialog would pass. Not once
    // it has focus: it does not take focus when it opens (its controls arrive with the snapshot),
    // and Escape is the window's, so it does not need to.
    await settle(
        `(()=>{const d=document.querySelector('[role=dialog]');const b=${button('Cancel')};return !!d&&!!b&&!b.disabled})()`,
        'The editor can be closed',
    );
    for (const type of ['keyDown', 'keyUp'])
        await call('Input.dispatchKeyEvent', {
            type,
            key: 'Escape',
            code: 'Escape',
            windowsVirtualKeyCode: 27,
        });
    await check(
        'Escape closes the editor',
        `document.querySelector('[role=dialog]')===null`,
    );
    await click('Plugins');
    await click('Develop a plugin');
    await check(
        'the contract inventory is one list, folded until asked for',
        `!!${button('Show the public contract')}&&!document.querySelector('input[aria-label="Search public contracts"]')`,
    );
    await click('Show the public contract');
    await check(
        'internal operations are left out of the inventory',
        `document.querySelectorAll('input[aria-label="Search public contracts"]').length===1&&!document.body.innerText.includes('claim_launch_restore')`,
    );
    await evaluate(
        `const i=document.querySelector('input[aria-label="Search public contracts"]');i.value='chat.';i.dispatchEvent(new Event('input',{bubbles:true}))`,
    );
    await check(
        'inventory can find chat contracts',
        `document.body.innerText.includes('ui.chat.edit')`,
    );
    await shot('wtm-extensions-inventory-light');
    await evaluate(
        `qa.settingsView.show('plugins','defaults')`,
    );
    await check(
        'a link naming defaults opens Workbench defaults',
        `document.querySelector('[role=tab][aria-selected="true"]')?.textContent.trim()==='Workbench defaults'`,
    );
    const pick = (id, value) =>
        evaluate(
            `(()=>{const s=document.querySelector('#${id}');s.value='${value}';s.dispatchEvent(new Event('change',{bubbles:true}))})()`,
        );
    await pick('plugin-slot-workbench', 'hidden');
    await check(
        'hiding the workbench asks first and says how to get it back',
        `(()=>{const d=[...document.querySelectorAll('[role=dialog]')].at(-1);return !!d&&d.innerText.includes('Plugin recovery')&&!qa.calls.some(c=>c.command==='plugin_bind')})()`,
    );
    await click('Cancel');
    await check(
        'a cancelled hide puts the select back',
        `document.querySelector('#plugin-slot-workbench').value==='builtin'`,
    );
    await evaluate(`qa.refuseBind=true`);
    await pick('plugin-slot-code', 'hidden');
    await check(
        'a refused region choice puts the select back',
        `document.querySelector('#plugin-slot-code').value==='builtin'&&document.body.innerText.includes('That view is not available.')`,
    );
    await evaluate(`qa.refuseBind=false`);
    console.log('errors', errors);
    if (errors.length) throw Error('Unexpected runtime exceptions');
} finally {
    await call('Page.close').catch(() => {});
    ws.close();
}

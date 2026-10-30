/** Exercise recipes in the window with mocked IPC: Settings › Recipes and Actions, the question an
 * action asks, the run strip, and the Remove dialog's "run it first". The runner itself is Rust and
 * is covered by `cargo test -p wtm-app recipes`; this probe opens no agent and spends no credits.
 *
 *   WTM_PLAYWRIGHT_MODULE=/path/to/playwright/index.mjs [WTM_SHOTS=dir] node scripts/check-recipes-ui.mjs
 */
import assert from 'node:assert/strict';
import { pathToFileURL } from 'node:url';
import { createServer } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

if (!process.env.WTM_PLAYWRIGHT_MODULE)
    throw new Error('Set WTM_PLAYWRIGHT_MODULE to an existing Playwright module.');
const { chromium } = await import(pathToFileURL(process.env.WTM_PLAYWRIGHT_MODULE).href);
const shots = process.env.WTM_SHOTS;

const fixture = `
const step=(o)=>({name:'',agent:'claude',model:null,effort:null,mode:null,kind:'prompt',skill:'',args:'',prompt:'',conversation:'new',previousReply:false,onFailure:'stop',...o});
window.checkState={calls:[],listeners:{},callbacks:{},next:0,closed:0,
 recipes:[
  {id:'implementation',name:'Implementation',description:'Plan the ticket, build it, then have Codex review it.',repositories:[],steps:[
   step({name:'Plan',model:'opus',effort:'high',mode:'plan',prompt:'Plan the work for {{form.ticket}} on {{worktree.branch}}.'}),
   step({name:'Implement',model:'opus',mode:'auto',conversation:'continue',prompt:'Implement the plan.'}),
   step({name:'Review',agent:'codex',mode:'read-only',kind:'skill',skill:'review',args:'Focus on {{form.ticket}}',previousReply:true})]},
  {id:'review',name:'Review',description:'',repositories:[],steps:[step({name:'Correctness',kind:'skill',skill:'code-review'})]},
  {id:'wrap-up',name:'Wrap up',description:'',repositories:[],steps:[step({name:'Check for unfinished work',mode:'plan',prompt:'List what would be lost.'})]}],
 offers:{'worktree.created':['implementation','review'],'worktree.removing':['wrap-up']}};
const state=window.checkState;
const all=()=>({recipes:structuredClone(state.recipes),problems:[{id:'broken',message:'invalid type: integer'}],offers:structuredClone(state.offers)});
const capability=(agent)=>({modelsAreLive:false,supportsFast:false,steersMidTurn:false,
 models:agent==='codex'?[{id:'gpt-5.6-sol',label:'gpt-5.6-sol',description:null,isDefault:true,impliedMode:null,defaultEffort:'high',efforts:[{effort:'low',description:null},{effort:'high',description:null}]}]
  :[{id:'opus',label:'Opus 5.5',description:null,isDefault:true,impliedMode:null,defaultEffort:'high',efforts:[{effort:'low',description:null},{effort:'high',description:null},{effort:'max',description:null}]}],
 modes:agent==='codex'?[{id:'read-only',label:'Read only',description:null,isDefault:false,risk:'normal'},{id:'auto',label:'Auto',description:null,isDefault:true,risk:'elevated'},{id:'full-access',label:'Full access',description:null,isDefault:false,risk:'unsandboxed'}]
  :[{id:'plan',label:'Plan',description:null,isDefault:false,risk:'normal'},{id:'auto',label:'Auto',description:null,isDefault:true,risk:'elevated'},{id:'bypassPermissions',label:'Bypass permissions',description:null,isDefault:false,risk:'unsandboxed'}]});
state.worktree={id:'/project/feat-topic',title:'feat-topic',subtitle:'',path:'/project/feat-topic',dirname:'feat-topic',branch:'feat/topic',head:null,isMain:false,isBare:false,locked:null,prunable:null,dirty:false,untracked:0,staged:0,ahead:0,behind:0,issueKey:null,badges:[],links:[],table:[],env:[]};
const run=(o)=>({id:'recipe-1-0',recipeId:'review',recipeName:'Review',projectId:'/project',worktreeId:state.worktree.id,action:'worktree.created',status:'running',startedMs:Date.now()-65000,endedMs:null,missing:[],
 steps:[{name:'Correctness',agent:'claude',model:null,mode:null,status:'running',session:'s1',error:null,startedMs:Date.now()-65000,endedMs:null}],...o});
window.emitRun=(o)=>{for(const [event,id] of Object.entries(state.listeners))if(event==='recipe:run')state.callbacks[id]?.({id,event,payload:run(o)})};
window.__TAURI_INTERNALS__={metadata:{currentWindow:{label:'main'},currentWebview:{label:'main'}},transformCallback:fn=>{const id=++state.next;state.callbacks[id]=fn;return id;},unregisterCallback:id=>delete state.callbacks[id],invoke:async(command,args)=>{
 state.calls.push({command,args:structuredClone(args)});
 if(command==='plugin:event|listen'){state.listeners[args.event]=args.handler;return ++state.next;}
 if(command==='plugin:event|unlisten')return;
 if(command==='list_recipes')return all();
 if(command==='save_recipe'){const r=structuredClone(args.recipe);if(!r.id){const base=r.name.toLowerCase().replace(/[^a-z0-9]+/g,'-').replace(/^-|-$/g,'')||'recipe';let id=base,n=2;while(state.recipes.some(x=>x.id===id))id=base+'-'+n++;r.id=id;}
  const at=state.recipes.findIndex(x=>x.id===r.id);if(at<0)state.recipes.push(r);else state.recipes[at]=r;return {id:r.id,all:all()};}
 if(command==='delete_recipe'){state.recipes=state.recipes.filter(r=>r.id!==args.id);return all();}
 if(command==='set_recipe_offers'){if(args.recipes.length)state.offers[args.action]=args.recipes;else delete state.offers[args.action];return all();}
 if(command==='agent_skills')return args.agent==='codex'?[{name:'repo-check',description:'A repository skill',scope:'repo'}]:[{name:'code-review',description:'Review the current diff',scope:'skill'}];
 // The CLI's own answer arrives later and has what the disk list cannot: a plugin's skills.
 if(command==='probe_agent_skills'){await new Promise((r)=>setTimeout(r,400));return args.agent==='codex'?[{name:'repo-check',description:'A repository skill',scope:'repo'},{name:'acme-tools:implement',description:'Implement a planned change end to end, with a long description that has to truncate',scope:'skill'}]:[{name:'code-review',description:'Review the current diff',scope:'skill'}];}
 if(command==='agent_capability')return capability(args.agentId);
 if(command==='list_agents')return [{id:'claude',label:'Claude Code',blurb:'',available:true,offered:true,detail:null,model:null,effort:null,mode:null},{id:'codex',label:'Codex',blurb:'',available:true,offered:true,detail:null,model:null,effort:null,mode:null}];
 if(command==='preview_recipe')return state.recipes.find(r=>r.id===args.recipeId).steps.map((s,i)=>({text:(s.kind==='skill'?'/'+s.skill+' '+s.args:s.prompt).replace('{{form.ticket}}',args.vars['form.ticket']??''),getsPrevious:i>0&&s.previousReply,missing:[]}));
 if(command==='start_recipe'){const r=run({recipeId:args.recipeId,recipeName:state.recipes.find(x=>x.id===args.recipeId).name,action:args.action,worktreeId:args.worktreeId});setTimeout(()=>window.emitRun(r),0);return r;}
 if(command==='stop_recipe'||command==='dismiss_recipe_run')return;
 if(command==='recipe_runs')return [];
 if(command==='list_worktrees')return [structuredClone(state.worktree)];
 if(command==='worktree_form')return {projectId:args.projectId,actions:[],fields:[{key:'issue',label:'Jira issue',kind:'text'},{key:'title',label:'Title override',kind:'text'}]};
 if(command==='remove_preflight')return [];
 if(command==='list_palettes')return [];
 if(command==='list_openers')return {openers:[],preferred:null};
 return null;
}};`;

const entry = `
import {mount} from 'svelte';
import '/src/styles/main.scss';
import {recipes} from '/src/lib/state/recipes.svelte.ts';
import {sessions} from '/src/lib/state/sessions.svelte.ts';
import {settingsView} from '/src/lib/state/settings-view.svelte.ts';
import {workspace} from '/src/lib/state/workspace.svelte.ts';
import {worktreeRemoval} from '/src/lib/state/worktree-removal.svelte.ts';
import {RECIPE_ACTIONS} from '/src/lib/recipes.ts';
import SettingsDialog from '/src/lib/components/SettingsDialog.svelte';
import RecipeOffer from '/src/lib/components/RecipeOffer.svelte';
import RecipeRunStrip from '/src/lib/components/RecipeRunStrip.svelte';
import RemoveWorktreeDialog from '/src/lib/components/RemoveWorktreeDialog.svelte';
const s=window.checkState;
workspace.projects=[{id:'/project',name:'project',root:'/project',usable:true,problem:null,trust:null}];
workspace.activeProjectId='/project';
await sessions.refreshOptions('/project');
await recipes.init();
window.removalTarget=()=>worktreeRemoval.target?.worktree.id??null;
const scenario=location.search.slice(1).split('&')[0];
const target=document.querySelector('#root');
if(scenario==='settings'){
 settingsView.show('recipes');
 mount(SettingsDialog,{target,props:{onclose:()=>{s.closed+=1;}}});
}else if(scenario==='offer'){
 target.innerHTML='<div id="strip"></div><div id="offer" style="padding:24px;max-width:880px"></div>';
 mount(RecipeRunStrip,{target:document.querySelector('#strip'),props:{worktreeId:s.worktree.id}});
 const show=()=>mount(RecipeOffer,{target:document.querySelector('#offer'),props:{action:RECIPE_ACTIONS[0],projectId:'/project',worktreeId:s.worktree.id,place:'feat-topic',vars:{'form.ticket':'ABC-1'},onstarted:()=>{s.started=true;},ondismiss:()=>{s.dismissed=true;}}});
 show();
}else if(scenario==='remove'){
 mount(RemoveWorktreeDialog,{target,props:{projectId:'/project',worktree:s.worktree,onclose:()=>{s.closed+=1;}}});
}
document.body.dataset.ready='yes';`;

const server = await createServer({
    configFile: false,
    envDir: false,
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
                    const theme = request.url.includes('light') ? 'light' : 'dark';
                    response.setHeader('Content-Type', 'text/html');
                    response.end(
                        `<!doctype html><html data-theme="${theme}" data-palette="pine"><body style="margin:0;height:100vh;background:var(--bg);color:var(--fg)"><div id="root"></div><script>` +
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
    const base = `http://127.0.0.1:${server.httpServer.address().port}/probe`;
    const errors = [];
    async function open(scenario, size = { width: 1360, height: 880 }) {
        const page = await browser.newPage({ viewport: size });
        page.on('pageerror', (e) => {
            errors.push(e.message);
            console.error('PAGE ERROR', e.message);
        });
        page.on('console', (m) => {
            if (m.type() === 'error') console.error('CONSOLE', m.text());
        });
        await page.goto(`${base}?${scenario}`);
        await page.waitForFunction(() => document.body.dataset.ready === 'yes');
        return page;
    }
    /** A step's summary button, by the step's own name rather than anything that contains it. */
    const stepCard = (page, name) =>
        page.locator('.c-recipe-step__summary').filter({
            has: page.locator('.c-recipe-step__title', {
                hasText: new RegExp(`^${name}$`),
            }),
        });
    const callsOf = (page, command) =>
        page.evaluate(
            (c) => window.checkState.calls.filter((x) => x.command === c),
            command,
        );
    const shoot = async (page, name) => {
        if (shots)
            await page.screenshot({ path: `${shots}/${name}.png`, animations: 'disabled' });
    };

    // ── Settings › Recipes ──
    let page = await open('settings');
    const sheet = page.locator('.c-dialog--sheet');
    await sheet.waitFor();
    assert.equal(
        await page.getByRole('tab', { name: /Recipes/ }).getAttribute('aria-selected'),
        'true',
        'Settings opens at the section it was asked for',
    );
    await page.getByRole('option', { name: /Implementation/ }).waitFor();
    await page.getByText('broken', { exact: true }).waitFor();
    assert.equal(await page.getByLabel('Recipe name').inputValue(), 'Implementation');
    await page.locator('.c-recipe__status', { hasText: 'Saved' }).waitFor();
    // Step 1 asks for {{form.ticket}}, which this repository's form does not have: the editor says
    // so, and offers the fields it does have.
    await page
        .getByText(
            '{{form.ticket}} won’t be filled: this repository’s New Worktree form has no “ticket” field; it has issue, title.',
        )
        .waitFor();
    await page.getByRole('button', { name: '{{form.issue}}', exact: true }).waitFor();
    await stepCard(page, 'Review').getByText('{{form.ticket}} won’t be filled').waitFor();
    await page.getByRole('button', { name: '{{worktree.issue}}', exact: true }).waitFor();
    await shoot(page, 'settings-recipes-dark');
    await page.getByText('won’t be filled').first().scrollIntoViewIfNeeded();
    await shoot(page, 'variables-warning-dark');

    // Open step 2 and rename it; the draft is dirty and Save carries exactly that change.
    await stepCard(page, 'Implement').click();
    const stepName = page.locator('#recipe-step-1-name');
    await stepName.fill('Build it');
    await page
        .locator('.c-recipe__status', { hasText: 'Unsaved changes to Implementation' })
        .waitFor();
    await page.keyboard.press('Escape');
    await page.getByRole('dialog', { name: 'Discard unsaved changes?' }).waitFor();
    await page.getByRole('button', { name: 'Keep editing' }).click();
    assert.equal(
        await page.evaluate(() => window.checkState.closed),
        0,
        'Escape asked rather than closed',
    );
    // Leaving for another recipe with changes asks; Discard really leaves.
    await page.getByRole('option', { name: /^Wrap up/ }).click();
    await page.getByRole('dialog', { name: 'Discard unsaved changes?' }).waitFor();
    await page.getByRole('button', { name: 'Keep editing' }).click();
    assert.equal(await page.getByLabel('Recipe name').inputValue(), 'Implementation');
    await page.getByRole('button', { name: 'Save recipe' }).click();
    await page.locator('.c-recipe__status', { hasText: 'Saved' }).waitFor();
    const saved = (await callsOf(page, 'save_recipe')).at(-1).args.recipe;
    assert.equal(saved.id, 'implementation');
    assert.deepEqual(
        saved.steps.map((s) => s.name),
        ['Plan', 'Build it', 'Review'],
    );

    // Step 3 runs on Codex, so it cannot continue step 2's Claude conversation; on Claude it can.
    await stepCard(page, 'Review').click();
    const cont = page.getByRole('radio', { name: /Continue step 2’s conversation/ });
    assert.equal(await cont.isDisabled(), true, 'a conversation cannot cross agents');
    await page.locator('#recipe-step-2-agent').selectOption('claude');
    assert.equal(await cont.isDisabled(), false);
    assert.equal(
        await page.locator('#recipe-step-2-model').inputValue(),
        '',
        'a new agent starts from its own defaults',
    );
    await page.locator('#recipe-step-2-mode').selectOption('bypassPermissions');
    await page.getByText('This mode runs with no approvals at all').waitFor();
    await page.getByRole('button', { name: 'Discard', exact: true }).click();
    await page.locator('.c-recipe__status', { hasText: 'Saved' }).waitFor();

    // The skill picker shows the disk list at once, then the CLI's own answer with a plugin's
    // skills in it, asked from the worktree in view.
    await stepCard(page, 'Review').click();
    await page.getByRole('option', { name: /repo-check/ }).waitFor();
    await page.getByRole('option', { name: /acme-tools:implement/ }).waitFor();
    assert(
        (await callsOf(page, 'probe_agent_skills')).some((c) => c.args.agent === 'codex'),
        "the step's own agent was asked for its list",
    );
    const list = page.locator('#recipe-step-2-edit .c-skill__list');
    const before = (await list.boundingBox()).height;
    // A leading slash, as people type it, still finds the namespaced plugin skill.
    const search = page.locator('#recipe-step-2-skill-search');
    await search.fill('/implement');
    await page.getByRole('option', { name: /acme-tools:implement/ }).waitFor();
    assert.equal(
        (await list.boundingBox()).height,
        before,
        'narrowing the matches does not shrink the list and move everything below it',
    );
    const row = page.getByRole('option', { name: /acme-tools:implement/ });
    const [name, desc] = await Promise.all([
        row.locator('.c-skill__name').boundingBox(),
        row.locator('.c-skill__desc').boundingBox(),
    ]);
    assert(
        name.x + name.width <= desc.x + 0.5,
        'a long name never runs under its description',
    );
    await shoot(page, 'skill-search-dark');
    await search.fill('plugin-skill');
    await page.getByText('No skill matches “plugin-skill”.').waitFor();
    await page.getByRole('option', { name: /\/plugin-skill/ }).click();
    await page.getByRole('button', { name: 'Save recipe' }).click();
    await page.locator('.c-recipe__status', { hasText: 'Saved' }).waitFor();
    assert.equal(
        (await callsOf(page, 'save_recipe')).at(-1).args.recipe.steps[2].skill,
        'plugin-skill',
    );

    // Discarding on the way to another recipe really goes there, with the draft thrown away.
    await stepCard(page, 'Plan').click();
    await page.locator('#recipe-step-0-name').fill('Plan it');
    await page.getByRole('option', { name: /^Wrap up/ }).click();
    await page.getByRole('button', { name: 'Discard', exact: true }).last().click();
    await page.waitForFunction(
        () => document.querySelector('[aria-label="Recipe name"]')?.value === 'Wrap up',
    );

    // A template is an unsaved draft until Save, which stores it under a new id.
    await page.getByRole('button', { name: 'Review', exact: true }).first().click();
    await page.locator('.c-recipe__status', { hasText: 'Not saved yet' }).waitFor();
    await page.getByRole('button', { name: 'Save recipe' }).click();
    await page.locator('.c-recipe__status', { hasText: 'Saved' }).waitFor();
    const fromTemplate = (await callsOf(page, 'save_recipe')).at(-1).args.recipe;
    assert.equal(fromTemplate.id, '', 'a new recipe is saved without an id and given one');
    assert.equal(fromTemplate.steps[1].agent, 'codex');

    // ── Settings › Actions ──
    await page.getByRole('tab', { name: /Actions/ }).click();
    await page.getByRole('option', { name: /Repository added/ }).click();
    await page.getByText('No recipes. Repository added won’t ask anything.').waitFor();
    await page.getByRole('combobox', { name: 'Recipe to offer' }).selectOption('wrap-up');
    await page.getByRole('button', { name: 'Offer', exact: true }).click();
    await page.getByText('What you’ll see').waitFor();
    assert.deepEqual((await callsOf(page, 'set_recipe_offers')).at(-1).args, {
        action: 'project.added',
        recipes: ['wrap-up'],
    });
    await page.getByRole('option', { name: /Worktree created/ }).click();
    await page.getByRole('button', { name: 'Offer Review earlier' }).click();
    assert.deepEqual((await callsOf(page, 'set_recipe_offers')).at(-1).args.recipes, [
        'review',
        'implementation',
    ]);
    await shoot(page, 'settings-actions-dark');
    // An action that offers one recipe takes a second; the question then asks which to run.
    await page.getByRole('option', { name: /Removing a worktree/ }).click();
    await page.getByRole('combobox', { name: 'Recipe to offer' }).selectOption('review');
    await page.getByRole('button', { name: 'Offer', exact: true }).click();
    assert.deepEqual((await callsOf(page, 'set_recipe_offers')).at(-1).args, {
        action: 'worktree.removing',
        recipes: ['wrap-up', 'review'],
    });
    await page.locator('.c-action__preview').getByRole('radio').nth(1).waitFor();
    // Offer the rest: with nothing left, the row says what to do instead of greying out.
    while ((await page.getByRole('combobox', { name: 'Recipe to offer' }).count()) > 0)
        await page.getByRole('button', { name: 'Offer', exact: true }).click();
    await page.getByText('Every recipe you have is offered here.').waitFor();
    await page.getByRole('button', { name: 'New recipe', exact: true }).last().click();
    await page.locator('.c-recipe__status', { hasText: 'Not saved yet' }).waitFor();
    assert.equal(
        await page.getByRole('tab', { name: /Recipes/ }).getAttribute('aria-selected'),
        'true',
    );
    await page.getByRole('tab', { name: /General/ }).click();
    await page.getByRole('dialog', { name: 'Discard unsaved changes?' }).waitFor();
    await page.getByRole('button', { name: 'Discard', exact: true }).last().click();
    await page.getByText('Send a message with').waitFor();
    await page.close();

    page = await open('settings&light', { width: 900, height: 760 });
    await page.locator('.c-dialog--sheet').waitFor();
    await page.getByRole('combobox', { name: 'Recipe', exact: true }).waitFor();
    assert.equal(
        await page.locator('.c-recipe-list').isVisible(),
        false,
        'a narrow sheet trades the list for a picker',
    );
    await shoot(page, 'settings-recipes-narrow-light');
    await page.close();

    // ── The question, and the run it starts ──
    page = await open('offer');
    await page.getByText('Run a recipe in').waitFor();
    assert.equal(
        await page.getByRole('radio', { name: /Implementation/ }).isChecked(),
        true,
    );
    await page.getByRole('button', { name: 'Show what it will send' }).click();
    await page.getByText('Plan the work for ABC-1').waitFor();
    await page.getByText('+ step 2’s reply').waitFor();
    await shoot(page, 'offer-dark');
    await page.getByRole('radio', { name: /^Review/ }).check();
    await page.getByRole('button', { name: 'Run Review' }).click();
    const started = (await callsOf(page, 'start_recipe')).at(-1).args;
    assert.deepEqual(started, {
        recipeId: 'review',
        projectId: '/project',
        worktreeId: '/project/feat-topic',
        action: 'worktree.created',
        vars: { 'form.ticket': 'ABC-1' },
    });
    await page.locator('.c-run-strip').getByText('step 1 of 1').waitFor();
    await page.getByRole('button', { name: /Stop recipe/ }).click();
    assert.equal((await callsOf(page, 'stop_recipe')).length, 1);
    await page.evaluate(() =>
        window.emitRun({
            status: 'stopped',
            endedMs: Date.now(),
            steps: [
                {
                    name: 'Correctness',
                    agent: 'claude',
                    model: null,
                    mode: null,
                    status: 'stopped',
                    session: 's1',
                    error: null,
                    startedMs: 0,
                    endedMs: 1,
                },
            ],
        }),
    );
    await page.getByRole('button', { name: /Dismiss/ }).waitFor();
    await shoot(page, 'strip-stopped-dark');
    await page.getByRole('button', { name: 'Not now' }).click();
    assert.equal(await page.evaluate(() => window.checkState.dismissed), true);
    await page.close();

    // ── Removing a worktree: run the recipe first, and the dialog comes back after ──
    page = await open('remove');
    await page.getByRole('button', { name: /Run Wrap up first/ }).click();
    const first = (await callsOf(page, 'start_recipe')).at(-1).args;
    assert.equal(first.action, 'worktree.removing');
    assert.equal(
        await page.evaluate(() => window.checkState.closed),
        1,
        'the dialog closes while it runs',
    );
    assert.equal(await page.evaluate(() => window.removalTarget()), null);
    await page.evaluate(() =>
        window.emitRun({
            recipeId: 'wrap-up',
            recipeName: 'Wrap up',
            action: 'worktree.removing',
            status: 'done',
            endedMs: Date.now(),
        }),
    );
    await page.waitForFunction(() => window.removalTarget() === '/project/feat-topic');
    assert.equal(
        (await callsOf(page, 'remove_worktree')).length,
        0,
        'a recipe finishing never removes anything by itself',
    );
    await page.close();

    assert.deepEqual(errors, []);
    console.log(
        'PASS: Settings sheet at a section, recipe edit/save/discard and the leave guard, cross-agent continue refused, defaults on agent change, unsandboxed warning, typed skill, template save, action offers and order, narrow picker, the question with what it sends, the run strip and Stop, and run-first removal that only reopens the dialog.',
    );
} finally {
    await browser?.close();
    await server.close();
}

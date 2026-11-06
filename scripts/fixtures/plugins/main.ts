import { mount } from 'svelte';
import Harness from './Harness.svelte';
import { plugins } from '/src/lib/state/plugins.svelte';
import { extensions } from '/src/lib/state/extensions.svelte';
import { workspace } from '/src/lib/state/workspace.svelte';
import { sessions } from '/src/lib/state/sessions.svelte';
import { chatEdit } from '/src/lib/state/chat-edit.svelte';
import '/src/styles/main.scss';
const qa = (window as any).qa;
plugins.state = qa.snapshot;
plugins.ready = true;
workspace.activeProjectId = 'qa-project';
workspace.selectedWorktreeId = '/tmp/qa-workspace';
workspace.worktrees = [
  { id: '/tmp/qa-workspace', title: 'Example workspace', path: '/tmp/qa-workspace' },
] as any;
sessions.panes = [
  {
    id: 'qa-pane',
    session: 'qa-session',
    kind: { kind: 'agent', provider: 'codex' },
    projectId: 'qa-project',
    worktreeId: '/tmp/qa-workspace',
    events: [],
    working: false,
    approvals: [],
  },
] as any;
sessions.showExtensionSession = async (...args) =>
  qa.calls.push({ command: 'show-branch', args });
sessions.focus = (...args) => qa.calls.push({ command: 'focus', args });
extensions.ui = async (operation, args) => {
  if (operation === 'ui.composer.read') return { text: qa.draft ?? '', revision: 1 };
  if (operation === 'ui.composer.write') {
    qa.draft = args.text;
    return { text: qa.draft, revision: 2 };
  }
  throw Error('Unexpected UI operation ' + operation);
};
Object.assign(qa, { plugins, extensions, chatEdit });
mount(Harness, { target: document.querySelector('#app')! });

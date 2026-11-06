import { mount } from 'svelte';
import Harness from './Harness.svelte';
import LaunchHarness from './LaunchHarness.svelte';
import { nativeSessions } from '/src/lib/state/native-sessions.svelte';
import { sessions } from '/src/lib/state/sessions.svelte';
import { theme } from '/src/lib/state/theme.svelte';
import { plugins } from '/src/lib/state/plugins.svelte';
import '@xterm/xterm/css/xterm.css';
import '/src/styles/main.scss';
const qa = (window as any).qa;
qa.launch = new URLSearchParams(location.search).has('launch');
const snapshot = {
  session: 'qa-native',
  project: 'example',
  worktree: '/tmp/example',
  provider: 'codex',
  conversation: 'example-conversation',
  status: 'idle',
  version: 'Codex · UI fixture',
  contextSupplied: true,
  toolsConnected: true,
  tools: [
    'Open Design',
    'Code review comments',
    'Browser panes',
    'Session awareness',
    'Agent delegation',
  ],
  capabilities: {
    send: true,
    stop: true,
    plans: true,
    usage: true,
    permissionOwner: 'native_cli',
  },
  title: 'Example design',
  model: 'example',
  plan: [
    { text: 'Read the existing navigation', status: 'completed' },
    { text: 'Prepare a design review', status: 'in_progress' },
  ],
  activity: ['Turn started', 'Using prototype_prepare', 'Tool finished'],
  usage: {
    tokensIn: 3200,
    tokensOut: 800,
    cached: 100,
    contextUsed: 3000,
    contextWindow: 100000,
  },
  deliveries: [],
  error: null,
  seq: 0,
};
qa.review = {
  prototype: {
    id: 'prototype',
    title: 'Example navigation',
    sources: [],
    review: { id: 'review', fingerprint: 'test', opened: true },
    feedback: [],
    phase: 'Ready for review',
    brief: 'Example',
    implementedReview: null,
    dismissed: false,
  },
  stale: false,
};
qa.native = nativeSessions;
qa.theme = theme;
qa.update = (patch: any) =>
  nativeSessions.accept({
    ...nativeSessions.states['qa-native'],
    ...patch,
    seq: nativeSessions.states['qa-native'].seq + 1,
  });
nativeSessions.accept(snapshot as any);
sessions.options = [
  { id: 'codex', label: 'Codex', available: true, offered: true },
  { id: 'claude', label: 'Claude', available: true, offered: true },
] as any;
if (!qa.launch) {
  sessions.restart = async () => {
    qa.calls.push({ command: 'reconnect' });
    qa.update({ status: 'idle', error: null });
  };
}
plugins.state = {
  registry: {
    installed: {},
    disabled: [],
    experience: 'wtm.native-cli',
    safe_mode: false,
    bindings: {},
    extensions: {},
    extension_choices: {},
    extension_order: [],
  },
  problem: null,
  builtins: [],
} as any;
Object.assign(qa, { sessions, plugins });
qa.newNative = (args: any) => {
  const session = !qa.launch && !qa.seeded ? 'qa-native' : `native-${++qa.n}`;
  qa.seeded = true;
  nativeSessions.accept({
    ...snapshot,
    session,
    provider: args.provider,
    conversation: `conversation-${session}`,
  } as any);
  return session;
};
qa.resetLaunch = (experience = 'wtm.native-cli') => {
  sessions.panes = [];
  sessions.layouts = {};
  sessions.focused = {};
  sessions.error = null;
  sessions.files = { '/tmp/example': [] };
  plugins.state.registry.experience = experience;
  qa.calls = [];
};
qa.reviews = [];
qa.publishReview = (patch = {}) => {
  qa.review = { ...qa.review, ...patch };
  qa.reviews = [qa.review];
  for (const id of qa.listeners['prototype:changed'] ?? []) {
    qa.callbacks[id]({ payload: { session: 'qa-native', state: qa.review } });
  }
};
qa.resetLaunch();
if (!qa.launch) await sessions.openAgent('example', '/tmp/example', 'codex');
mount(qa.launch ? LaunchHarness : Harness, { target: document.querySelector('#app')! });

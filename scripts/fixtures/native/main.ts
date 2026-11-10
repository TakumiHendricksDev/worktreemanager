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
    review: { id: 'review', fingerprint: 'test', opened: false },
    feedback: [],
    phase: 'ready for review',
    brief: 'Example',
    implementedReview: null,
    dismissed: false,
  },
  stale: false,
  delivery: null,
};
qa.native = nativeSessions;
qa.theme = theme;
/** The integrated pane's session now. A reconnect replaces it, as the real restart does. */
qa.current = () => sessions.panes[0]?.session ?? 'qa-native';
qa.update = (patch: any) => {
  const state = nativeSessions.states[qa.current()];
  nativeSessions.accept({ ...state, ...patch, seq: state.seq + 1 });
};
nativeSessions.accept(snapshot as any);
sessions.options = [
  { id: 'codex', label: 'Codex', available: true, offered: true },
  { id: 'claude', label: 'Claude Code', available: true, offered: true },
  // No native adapter: the dialog has to fall back for it, or say nothing can open it.
  { id: 'cursor', label: 'Cursor Agent', available: true, offered: true },
] as any;
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
/**
 * What Rust still knows about a conversation after its process is closed: the provider it runs
 * on and its delivery receipts, which live in a journal keyed by conversation. `close_terminal`
 * files a native session here, and `open_native_cli` with `resume` takes it back, so a reconnect
 * comes back to the same receipts it left, and nothing it sends can hide behind a fresh state.
 */
qa.journal = {};
qa.newNative = (args: any) => {
  const session = !qa.launch && !qa.seeded ? 'qa-native' : `native-${++qa.n}`;
  qa.seeded = true;
  const resumed = args.resume ? qa.journal[args.resume] : undefined;
  nativeSessions.accept(
    (resumed
      ? { ...resumed, session, status: 'idle', error: null, seq: 0 }
      : {
          ...snapshot,
          session,
          provider: args.provider,
          conversation: `conversation-${session}`,
        }) as any,
  );
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
    qa.callbacks[id]({ payload: { session: qa.current(), state: qa.review } });
  }
};
qa.resetLaunch();
if (!qa.launch) await sessions.openAgent('example', '/tmp/example', 'codex');
mount(qa.launch ? LaunchHarness : Harness, { target: document.querySelector('#app')! });

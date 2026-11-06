import { listen } from '@tauri-apps/api/event';
import { commands } from '../ipc/commands';
import type { NativeSession } from '../native-sessions';

class NativeSessions {
  states = $state<Record<string, NativeSession>>({});

  accept(state: NativeSession) {
    if ((this.states[state.session]?.seq ?? -1) > state.seq) return false;
    this.states = { ...this.states, [state.session]: state };
    return true;
  }

  async load(session: string) {
    const state = await commands.nativeSession(session);
    this.accept(state);
    return this.states[session] ?? state;
  }

  async init(changed: (state: NativeSession) => void) {
    const off = await listen<NativeSession>('native:changed', ({ payload }) => {
      if (this.accept(payload)) changed(payload);
    });
    const states = await commands.nativeSessions().catch(() => []);
    for (const state of states) {
      if (this.accept(state)) changed(state);
    }
    return off;
  }
}

export const nativeSessions = new NativeSessions();

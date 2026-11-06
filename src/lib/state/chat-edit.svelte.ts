import { commands } from '../ipc/commands';
import { errorMessage } from '../ipc/types';
import { sessions } from './sessions.svelte';
import { extensions } from './extensions.svelte';

export type ChatEditMessage = {
  id: string;
  turn: string;
  text: string;
  previous: string | null;
  problem: string | null;
};
export type ChatEditSnapshot = {
  session: string;
  revision: number;
  messages: ChatEditMessage[];
  problem: string | null;
  status: 'ready' | 'busy' | 'unsupported';
};
export type ChatEditRequest = {
  id: string;
  session: string;
  message: string;
  revision: number;
  text: string;
};
export type ChatEditOutcome = { session: string; original: string; submitted: boolean };
class ChatEdit {
  opened = $state(false);
  busy = $state(false);
  problem = $state('');
  originalPane = $state('');
  snapshot = $state<ChatEditSnapshot | null>(null);
  message = $state<ChatEditMessage | null>(null);
  text = $state('');
  outcome = $state<ChatEditOutcome | null>(null);
  private request: ChatEditRequest | null = null;
  private selectedMessage = '';
  private fallbackText = '';
  stopRequested = $state(false);
  attempted = $state(false);
  async open(paneId: string, messageId: string, fallbackText = '') {
    if (this.busy) throw Error('An edit is already being prepared');
    const pane = sessions.paneById(paneId);
    if (!pane?.session) throw Error('Session is not live');
    this.originalPane = paneId;
    this.selectedMessage = messageId;
    this.fallbackText = fallbackText;
    this.stopRequested = false;
    this.attempted = false;
    this.opened = true;
    this.busy = true;
    this.problem = '';
    this.outcome = null;
    this.snapshot = null;
    this.message = null;
    this.request = null;
    try {
      this.snapshot = await commands.chatEditSnapshot(pane.session);
      this.message = this.snapshot.messages.find((m) => m.id === messageId) ?? null;
      this.problem =
        this.snapshot.problem ??
        this.message?.problem ??
        (!this.message
          ? 'This message has no exact provider boundary. Editing is not available.'
          : '');
      this.text = this.message?.text ?? fallbackText;
    } catch (e) {
      this.problem = errorMessage(e);
    } finally {
      this.busy = false;
    }
  }
  async stopOrRefresh() {
    if (this.busy || !this.snapshot) return;
    this.busy = true;
    try {
      if (!this.stopRequested) await commands.interruptTurn(this.snapshot.session);
      this.stopRequested = true;
      this.snapshot = await commands.chatEditSnapshot(this.snapshot.session);
      this.message =
        this.snapshot.messages.find((m) => m.id === this.selectedMessage) ?? null;
      this.problem =
        this.snapshot.problem ??
        this.message?.problem ??
        (!this.message ? 'This message has no exact provider boundary.' : '');
    } catch (e) {
      this.problem = errorMessage(e);
    } finally {
      this.busy = false;
    }
  }
  async copyToComposer() {
    const pane = sessions.paneById(this.originalPane);
    const text = this.text || this.fallbackText;
    if (this.busy || !pane || !text || !extensions.ui) return;
    this.busy = true;
    try {
      const context = {
        projectId: pane.projectId,
        worktreeId: pane.worktreeId,
        paneId: pane.id,
      };
      const current = (await extensions.ui(
        'ui.composer.read',
        { paneId: pane.id },
        context,
      )) as { text: string; revision: number };
      await extensions.ui(
        'ui.composer.write',
        {
          paneId: pane.id,
          revision: current.revision,
          text: current.text ? `${current.text}\n\n${text}` : text,
        },
        context,
      );
      sessions.focus(pane.worktreeId, pane.id);
      this.opened = false;
    } catch (e) {
      this.problem = errorMessage(e);
    } finally {
      this.busy = false;
    }
  }
  async submit() {
    if (this.busy || !this.snapshot || !this.message || this.problem || !this.text.trim())
      return;
    const pane = sessions.paneById(this.originalPane);
    if (!pane || pane.kind.kind !== 'agent') return;
    this.busy = true;
    // Preserve this id if the response is lost; the host journal never resends a submitted turn.
    this.attempted = true;
    this.request ??= {
      id: crypto.randomUUID(),
      session: this.snapshot.session,
      message: this.message.id,
      revision: this.snapshot.revision,
      text: this.text,
    };
    try {
      this.outcome = await commands.chatEdit(this.request);
      await sessions.showExtensionSession(
        pane.projectId,
        pane.worktreeId,
        pane.kind.provider,
        this.outcome.session,
      );
      if (!this.outcome.submitted)
        this.problem =
          'The branch was created, but submission has an uncertain outcome. Inspect it before sending anything again.';
    } catch (e) {
      this.problem = errorMessage(e);
    } finally {
      this.busy = false;
    }
  }
  close() {
    if (!this.busy) this.opened = false;
  }
}
export const chatEdit = new ChatEdit();

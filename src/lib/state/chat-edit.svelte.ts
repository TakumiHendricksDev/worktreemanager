/**
 * Edit and continue, as a store: the edit under review, and the one request that sends it.
 *
 * # One request per edit
 *
 * The first Send mints a request id and Rust journals it before anything is spawned, so the same
 * id can only ever have one outcome. Retry sends that same request again: after a failure before
 * the new conversation opened, Rust answers with the original error, and after one that came later
 * it answers with the conversation it opened, never sending the message a second time. That is
 * also why the text is locked once a request is held. Editing it would need a new id, and a new id
 * after an attempt whose outcome is unknown is exactly the duplicate send the journal prevents.
 * Choosing Edit again starts over with a fresh request.
 *
 * A conversation opened by a request that then failed arrives through `agent:spawned`, which the
 * sessions store already adopts into its worktree like any other announced session. Retry then
 * returns it as the outcome, and `showExtensionSession` focuses the pane that is already there.
 */
import { commands } from '../ipc/commands';
import {
  errorMessage,
  type ChatEditMessage,
  type ChatEditOutcome,
  type ChatEditRequest,
  type ChatEditSnapshot,
} from '../ipc/types';
import { sessions } from './sessions.svelte';
import { extensions } from './extensions.svelte';

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
    if (
      this.busy ||
      this.attempted ||
      !this.snapshot ||
      !this.message ||
      this.problem ||
      !this.text.trim()
    )
      return;
    // Kept if the response is lost; the host journal never resends a submitted turn.
    this.request = {
      id: crypto.randomUUID(),
      session: this.snapshot.session,
      message: this.message.id,
      revision: this.snapshot.revision,
      text: this.text,
    };
    this.attempted = true;
    await this.send();
  }
  /** Ask again with the same request. See the header for what Rust answers. */
  async retry() {
    if (this.busy || !this.request || this.outcome) return;
    this.problem = '';
    await this.send();
  }
  private async send() {
    const pane = sessions.paneById(this.originalPane);
    const request = this.request;
    if (!pane || pane.kind.kind !== 'agent' || !request) {
      this.problem = 'The original conversation is no longer open.';
      return;
    }
    this.busy = true;
    try {
      this.outcome = await commands.chatEdit(request);
      await sessions.showExtensionSession(
        pane.projectId,
        pane.worktreeId,
        pane.kind.provider,
        this.outcome.session,
      );
      if (!this.outcome.submitted)
        this.problem =
          'The new conversation opened, but whether your message reached it is unknown. Look at it before sending anything again.';
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

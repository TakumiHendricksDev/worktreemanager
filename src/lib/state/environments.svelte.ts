/**
 * Worktrees' environments as the Work view shows them: each one's services, commands and runs, and
 * a service's log once asked for.
 *
 * # Events, then a fresh read
 *
 * `env:changed` carries only the project and worktree. The store asks for the whole environment
 * again rather than patching it, for `review-runs.svelte.ts`'s reason. A health check that answers,
 * a server that exits and a command that finishes are all announced; nothing here runs on a timer
 * (ARCHITECTURE §8). A log is read when you open it, and again when its service changes, which is
 * when a log has something new worth reading.
 */

import { listen } from '@tauri-apps/api/event';

import { commands } from '../ipc/commands';
import type {
  EnvironmentView,
  PendingCommandView,
  ServiceLog,
  VerifiedPreview,
} from '../ipc/types';

function message(error: unknown): string {
  if (typeof error === 'object' && error !== null && 'message' in error) {
    return String((error as { message: unknown }).message);
  }
  return String(error);
}

class Environments {
  /** Each worktree's environment, by worktree id, once asked for. */
  views = $state<Record<string, EnvironmentView>>({});
  /** Why the last action on a worktree's environment failed, by worktree id. */
  errors = $state<Record<string, string>>({});
  /** Worktrees with an action in flight, so a button cannot be pressed twice. */
  busy = $state<Record<string, boolean>>({});
  /** The log shown, by worktree id: whose it is and what it says. */
  logs = $state<Record<string, { service: string; log: ServiceLog | null }>>({});
  /** The command waiting for approval in each worktree, by worktree id. */
  pending = $state<Record<string, PendingCommandView | null>>({});

  /** Follow the supervisor's announcements. Returns the unlisten. */
  async init(): Promise<() => void> {
    return listen<{ project: string; worktree: string }>('env:changed', (event) => {
      const { project, worktree } = event.payload;
      if (this.views[worktree]) void this.load(project, worktree);
    });
  }

  async load(project: string, worktree: string): Promise<void> {
    try {
      const view = await commands.environment(project, worktree);
      this.views[worktree] = view;
      await this.loadPending(view);
      const open = this.logs[worktree];
      if (open) await this.readLog(project, worktree, open.service);
    } catch (error) {
      this.errors[worktree] = message(error);
    }
  }

  private async loadPending(view: EnvironmentView): Promise<void> {
    const waiting = view.runs.find((run) => run.state === 'awaiting_approval');
    this.pending[view.worktree] = waiting
      ? await commands.pendingEnvironmentCommand(waiting.flow)
      : null;
  }

  private async act(
    project: string,
    worktree: string,
    action: () => Promise<EnvironmentView>,
  ): Promise<void> {
    if (this.busy[worktree]) return;
    this.busy[worktree] = true;
    delete this.errors[worktree];
    try {
      const view = await action();
      this.views[worktree] = view;
      await this.loadPending(view);
    } catch (error) {
      this.errors[worktree] = message(error);
      // A refused start still changed something worth showing: the conflict it found.
      await this.load(project, worktree);
    } finally {
      this.busy[worktree] = false;
    }
  }

  start(project: string, worktree: string, service: string | null): Promise<void> {
    return this.act(project, worktree, () =>
      commands.startServices(project, worktree, service),
    );
  }

  stop(project: string, worktree: string, service: string | null): Promise<void> {
    return this.act(project, worktree, () =>
      commands.stopServices(project, worktree, service),
    );
  }

  restart(project: string, worktree: string, service: string | null): Promise<void> {
    return this.act(project, worktree, () =>
      commands.restartServices(project, worktree, service),
    );
  }

  check(project: string, worktree: string, service: string): Promise<void> {
    return this.act(project, worktree, () =>
      commands.checkService(project, worktree, service),
    );
  }

  run(project: string, worktree: string, command: string): Promise<void> {
    return this.act(project, worktree, () =>
      commands.runEnvironmentCommand(project, worktree, command),
    );
  }

  async approve(project: string, worktree: string, run: number): Promise<void> {
    await this.act(project, worktree, async () => {
      await commands.approveEnvironmentCommand(run);
      return commands.environment(project, worktree);
    });
  }

  async decline(project: string, worktree: string, run: number): Promise<void> {
    await this.act(project, worktree, async () => {
      await commands.declineEnvironmentCommand(run);
      return commands.environment(project, worktree);
    });
  }

  async setIdleStop(
    project: string,
    worktree: string,
    minutes: number | null,
  ): Promise<void> {
    await this.act(project, worktree, async () => {
      await commands.setIdleStop(project, minutes);
      return commands.environment(project, worktree);
    });
  }

  /**
   * A healthy service's preview, verified just now. `null`, with the reason recorded, when it did
   * not answer: the caller opens only what this returns.
   */
  async verify(
    project: string,
    worktree: string,
    service: string,
  ): Promise<VerifiedPreview | null> {
    delete this.errors[worktree];
    try {
      return await commands.verifyServicePreview(project, worktree, service);
    } catch (error) {
      this.errors[worktree] = message(error);
      return null;
    }
  }

  /** Show a service's log, or hide it when it is the one showing. */
  async toggleLog(project: string, worktree: string, service: string): Promise<void> {
    if (this.logs[worktree]?.service === service) {
      delete this.logs[worktree];
      return;
    }
    this.logs[worktree] = { service, log: null };
    await this.readLog(project, worktree, service);
  }

  async readLog(project: string, worktree: string, service: string): Promise<void> {
    try {
      const log = await commands.serviceLog(project, worktree, service);
      if (this.logs[worktree]?.service === service) this.logs[worktree] = { service, log };
    } catch (error) {
      this.errors[worktree] = message(error);
    }
  }
}

export const environments = new Environments();

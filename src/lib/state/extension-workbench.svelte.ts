/**
 * The frontend-owned public operations, `ui.*`: panels, the composer, chat edits, navigation and
 * Settings, as plugins and hooks see them.
 *
 * Each case validates what it is given against the context it runs in, because the arguments may
 * have come from a package or been rewritten by a hook. Focus, move, resize and navigation call
 * the stores' `apply*` methods, which skip interception: by the time a case runs, the hooks for
 * its operation already have.
 */
import { listen } from '@tauri-apps/api/event';
import type { ExtensionContext, WorkbenchPanel } from '../extensions';
import { commands } from '../ipc/commands';
import { errorMessage } from '../ipc/types';
import { inPaneWindow } from '../window-role';
import { chatEdit } from './chat-edit.svelte';
import { extensions } from './extensions.svelte';
import type { Target } from './layout.svelte';
import { sessions, type Pane } from './sessions.svelte';
import { settingsView, type SettingsSection } from './settings-view.svelte';
import { isMainView, view } from './view.svelte';
import { workspace } from './workspace.svelte';
import { workspaceLabels } from './workspace-labels.svelte';

type Composer = {
  read: () => { text: string; revision: number };
  write: (text: string, revision: number) => void;
};
const composers = new Map<string, Composer>();
export function registerExtensionComposer(paneId: string, composer: Composer): () => void {
  composers.set(paneId, composer);
  return () => {
    if (composers.get(paneId) === composer) composers.delete(paneId);
  };
}
function paneFor(args: Record<string, unknown>, context: ExtensionContext): Pane {
  const id = String(args.paneId ?? context.paneId ?? '');
  const pane = sessions.paneById(id);
  if (
    !pane ||
    pane.projectId !== context.projectId ||
    pane.worktreeId !== context.worktreeId
  )
    throw Error('Panel is no longer in this worktree');
  return pane;
}
function string(args: Record<string, unknown>, key: string): string {
  if (typeof args[key] !== 'string') throw Error(`${key} must be text`);
  return args[key];
}
function snapshot(pane: Pane): WorkbenchPanel {
  return {
    id: pane.id,
    session: pane.session,
    projectId: pane.projectId,
    worktreeId: pane.worktreeId,
    kind: pane.kind,
    title: pane.agentTitle,
    working: pane.working,
    ended: pane.ended,
  };
}
async function dispatch(
  operation: string,
  args: Record<string, unknown>,
  context: ExtensionContext,
): Promise<unknown> {
  switch (operation) {
    case 'ui.panels.list':
      return sessions.panes
        .filter(
          (p) => p.projectId === context.projectId && p.worktreeId === context.worktreeId,
        )
        .map(snapshot);
    case 'ui.panels.focus': {
      const p = paneFor(args, context);
      sessions.applyFocus(p.worktreeId, p.id);
      return snapshot(p);
    }
    case 'ui.panels.move': {
      const p = paneFor(args, context),
        target = args.target as Target | undefined;
      if (!target || !['pane', 'swap', 'surface'].includes(target.kind))
        throw Error('Invalid layout target');
      if ('paneId' in target) {
        const destination = sessions.paneById(target.paneId);
        if (!destination || destination.worktreeId !== p.worktreeId)
          throw Error('Destination is outside this worktree');
      }
      if ('side' in target && !['left', 'right', 'above', 'below'].includes(target.side))
        throw Error('Invalid placement');
      sessions.applyMove(p.worktreeId, p.id, target);
      return snapshot(p);
    }
    case 'ui.panels.resize': {
      const path = string(args, 'path');
      if (
        !/^[ab]*$/.test(path) ||
        typeof args.ratio !== 'number' ||
        !Number.isFinite(args.ratio)
      )
        throw Error('Invalid split path or ratio');
      const ratio = Math.min(0.9, Math.max(0.1, args.ratio));
      sessions.applyRatio(context.worktreeId, path, ratio);
      return { path, ratio };
    }
    case 'ui.panels.close': {
      const p = paneFor(args, context);
      if (p.working || p.approvals.length)
        throw Error('Panel is busy or awaiting approval. Stop it before closing.');
      await sessions.close(p.id);
      return { closed: p.id };
    }
    case 'ui.panels.open': {
      const kind = string(args, 'kind');
      if (kind === 'chat')
        return sessions.openChat(
          context.projectId,
          context.worktreeId,
          string(args, 'provider'),
        );
      if (kind === 'shell')
        return sessions.openShell(context.projectId, context.worktreeId);
      if (kind === 'browser')
        return sessions.openBrowser(
          context.projectId,
          context.worktreeId,
          typeof args.url === 'string' ? args.url : undefined,
        );
      throw Error(
        'Unknown panel kind. Custom HTML panels are declared in the plugin manifest.',
      );
    }
    case 'ui.workspace.rename': {
      const worktree = workspace.worktrees.find((w) => w.id === context.worktreeId);
      if (workspace.activeProjectId !== context.projectId || !worktree)
        throw Error('Workspace is no longer selected');
      const name = string(args, 'name');
      if (name.length > 256) throw Error('Workspace label is too long');
      await workspaceLabels.rename(worktree.path, name);
      return { id: worktree.id, name, path: worktree.path };
    }
    case 'ui.navigate': {
      const target = string(args, 'target');
      if (!isMainView(target)) throw Error('Unknown view');
      view.apply(target);
      return { target: view.current };
    }
    case 'ui.settings.show': {
      const section = string(args, 'section');
      if (
        ![
          'plugins',
          'appearance',
          'general',
          'notifications',
          'advanced',
          'recipes',
          'actions',
        ].includes(section)
      )
        throw Error('Unknown settings section');
      settingsView.show(
        section as SettingsSection,
        typeof args.focus === 'string' ? args.focus : null,
      );
      return { section };
    }
    case 'ui.composer.read':
    case 'ui.composer.write': {
      const p = paneFor(args, context),
        composer = composers.get(p.id);
      if (!composer) throw Error('Composer is not mounted in this window');
      if (operation.endsWith('write')) {
        if (typeof args.revision !== 'number')
          throw Error('Read the composer revision before replacing a draft');
        composer.write(string(args, 'text'), args.revision);
      }
      return composer.read();
    }
    case 'ui.chat.snapshot': {
      const p = paneFor(args, context);
      if (!p.session) throw Error('Session is not live');
      return {
        ...snapshot(p),
        ...(await commands.chatEditSnapshot(p.session)),
        events: $state.snapshot(p.events),
      };
    }
    case 'ui.chat.edit': {
      const p = paneFor(args, context);
      const message = String(args.messageId ?? context.messageId ?? '');
      // The text the review falls back to when the provider's snapshot lacks the message, found
      // here rather than taken from the context, which no longer carries what a message says.
      let text = '';
      for (let at = p.events.length - 1; at >= 0; at -= 1) {
        const event = p.events[at];
        if (event?.kind === 'message_identity' && event.message === message) {
          text = event.text;
          break;
        }
      }
      // Awaited, so a refusal ("An edit is already being prepared", "Session is not live")
      // reaches the plugin that asked rather than being dropped behind `{ opened: true }`. A
      // problem found once the dialog is open is shown in the dialog instead.
      await chatEdit.open(p.id, message, text);
      return { opened: true };
    }
    default:
      throw Error(`Workbench operation is not available: ${operation}`);
  }
}
/**
 * Make `ui.*` dispatchable, load workspace labels, and start the registry.
 *
 * Never rejects. It runs during boot, and a malformed config or a plugin that will not start is
 * not worth a window that never finishes opening: the workbench runs without plugins, and the
 * problem is recorded for Settings → Plugins.
 */
export async function initExtensionWorkbench(): Promise<() => void> {
  extensions.ui = dispatch;
  const offs: (() => void)[] = [];
  try {
    await workspaceLabels.init();
    offs.push(
      await listen<{ key: string }>('wtm:pref-changed', (event) => {
        if (event.payload.key === 'ui.workspace_labels') void workspaceLabels.init();
      }),
    );
    offs.push(await extensions.init());
    if (!inPaneWindow)
      offs.push(
        $effect.root(() => {
          // Each effect reads the panes only once a package subscribes, so without one a pane's
          // every status change is not also an IPC call.
          $effect(() => {
            if (!extensions.subscribed('workspace.changed')) return;
            const context = extensions.context();
            void commands
              .extensionEvent('workspace.changed', context, context.worktreeId)
              .catch(() => {});
          });
          $effect(() => {
            if (!extensions.subscribed('panels.changed')) return;
            const context = extensions.context();
            const panels = sessions.panes
              .filter((p) => p.worktreeId === context.worktreeId)
              .map(snapshot);
            void commands
              .extensionEvent('panels.changed', { context, panels }, context.worktreeId)
              .catch(() => {});
          });
        }),
      );
  } catch (e) {
    extensions.problems.host = errorMessage(e);
  }
  return () => {
    for (const off of offs.reverse()) off();
    extensions.ui = null;
  };
}

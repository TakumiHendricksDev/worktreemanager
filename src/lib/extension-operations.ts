/**
 * Host focus, navigation and layout, offered to plugin hooks and replacements.
 *
 * The stores call `interceptUiOperation` at the top of `focus`, `movePane`, `setRatio` and
 * `view.show`. It lives apart from the registry because those stores are imported by it, and an
 * import back would be a cycle.
 *
 * # Why a call is only asynchronous when something intercepts it
 *
 * A hook is a round trip to a plugin process, so an intercepted call cannot finish before it
 * returns. Callers read the state they just changed — a focus followed by a caret move, a drag
 * followed by a layout read — so with no hook or chosen replacement the answer is `false` and the
 * store applies the change synchronously, exactly as it did before plugins existed.
 *
 * # Why intercepted calls queue per operation
 *
 * A splitter sends `ui.panels.resize` on every pointer move. Each one waits on hooks of its own, and
 * two in flight at once could settle in either order and leave the split at an earlier ratio than
 * the pointer. Queuing per operation applies them in the order they were asked for.
 *
 * # Why there is no re-entrancy guard
 *
 * The dispatcher applies an operation through the store's un-intercepted method (`applyFocus`,
 * `view.apply`, …), so applying one cannot intercept it again. A counter used to do that, and it
 * covered only the synchronous start of an asynchronous dispatch.
 */
import type { ExtensionContext } from './extensions';

type Args = Record<string, unknown>;

export type UiInterceptor = {
  /** Whether a hook or chosen replacement applies. Must not track reactive reads. */
  intercepts(operation: string): boolean;
  /** Fill in a caller's empty workspace from what is on screen at the moment of the call. */
  context(partial: ExtensionContext): ExtensionContext;
  /** Run hooks, then apply. Reports its own failures, and never rejects. */
  run(operation: string, args: Args, context: ExtensionContext): Promise<void>;
};

let interceptor: UiInterceptor | null = null;
const queues = new Map<string, Promise<void>>();

export function setUiOperationInterceptor(value: UiInterceptor | null) {
  interceptor = value;
}

/** True when the operation was taken over; the caller then does nothing more. */
export function interceptUiOperation(
  operation: string,
  args: Args,
  context: ExtensionContext,
): boolean {
  const current = interceptor;
  if (!current?.intercepts(operation)) return false;
  const resolved = current.context(context);
  const queued = (queues.get(operation) ?? Promise.resolve())
    .then(() => current.run(operation, args, resolved))
    .catch(() => {});
  queues.set(operation, queued);
  void queued.then(() => {
    if (queues.get(operation) === queued) queues.delete(operation);
  });
  return true;
}

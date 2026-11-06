import type { ExtensionContext } from './extensions';

type Args = Record<string, unknown>;
type Interceptor = (
  operation: string,
  args: Args,
  context: ExtensionContext,
  apply: (args: Args) => void,
) => boolean;
let interceptor: Interceptor | null = null;
let applying = 0;
export function setUiOperationInterceptor(value: Interceptor | null) {
  interceptor = value;
}
export function withoutUiInterception<T>(action: () => T): T {
  applying += 1;
  try {
    return action();
  } finally {
    applying -= 1;
  }
}
/** Without an applicable registration, ordinary focus and drag operations stay synchronous. */
export function interceptUiOperation(
  operation: string,
  args: Args,
  context: ExtensionContext,
  apply: (args: Args) => void,
): boolean {
  return (
    applying === 0 &&
    (interceptor?.(operation, args, context, (next) =>
      withoutUiInterception(() => apply(next)),
    ) ??
      false)
  );
}

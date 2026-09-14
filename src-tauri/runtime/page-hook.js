/*
 * The page-world hook. Installed by wtm into every page a browser pane shows, in the page's
 * OWN JavaScript scope — which is the one scope the page can tamper with, and the reason this
 * file does as little as it can.
 *
 * It exists because two things are only observable from inside the page world: what the page
 * writes to its console, and the History API calls a single-page app uses to change its address
 * without loading anything. Both are reported to Rust over a message handler WebKit registers in
 * this world, and Rust treats every message as untrusted: console lines are shown as text an
 * agent asked to see, and a `navigated` report only prompts Rust to re-read the real URL from
 * the webview, never to believe the page.
 *
 * Bounded, deliberately: a page that logs in a loop gets two hundred lines through and then
 * silence until the next load. The agent runtime proper lives in an isolated content world —
 * see `browser.js` — and nothing here can reach it.
 */
(() => {
  if (window.__wtmPageHook) return;
  window.__wtmPageHook = true;

  const post = (message) => {
    try {
      window.webkit.messageHandlers.wtmPage.postMessage(JSON.stringify(message));
    } catch {
      /* The handler is not there (a frame, or a page that has been detached). Nothing to do. */
    }
  };

  const LIMIT = 200;
  const MAX_TEXT = 2000;
  let budget = LIMIT;

  const describe = (value) => {
    if (typeof value === 'string') return value;
    if (value instanceof Error) return `${value.name}: ${value.message}`;
    try {
      return JSON.stringify(value);
    } catch {
      return String(value);
    }
  };

  const report = (level, text) => {
    if (budget <= 0) return;
    budget -= 1;
    post({ event: 'console', level, text: text.slice(0, MAX_TEXT) });
    if (budget === 0)
      post({
        event: 'console',
        level: 'info',
        text: `[wtm] console output paused after ${LIMIT} lines on this page`,
      });
  };

  for (const level of ['log', 'info', 'warn', 'error', 'debug']) {
    const original = console[level];
    console[level] = function (...args) {
      report(level, args.map(describe).join(' '));
      return original.apply(this, args);
    };
  }

  window.addEventListener('error', (event) => {
    const where = event.filename ? ` (${event.filename}:${event.lineno})` : '';
    report('error', `${event.message}${where}`);
  });
  window.addEventListener('unhandledrejection', (event) => {
    report('error', `Unhandled promise rejection: ${describe(event.reason)}`);
  });

  const wrap = (name) => {
    const original = history[name];
    history[name] = function (...args) {
      const result = original.apply(this, args);
      post({ event: 'navigated' });
      return result;
    };
  };
  wrap('pushState');
  wrap('replaceState');
  window.addEventListener('popstate', () => post({ event: 'navigated' }));
  window.addEventListener('hashchange', () => post({ event: 'navigated' }));
})();

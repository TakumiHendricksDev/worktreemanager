/* An opaque frame can only ask its owner; no Tauri IPC or privileged DOM is exposed. */
(() => {
  const pending = new Map();
  let sequence = 0;
  addEventListener('message', (event) => {
    if (event.source === parent && event.data?.channel === 'wtm.plugin.theme') {
      for (const [key, value] of Object.entries(event.data.tokens)) {
        if (/^[a-z-]+$/.test(key) && typeof value === 'string')
          document.documentElement.style.setProperty(`--wtm-${key}`, value);
      }
      dispatchEvent(new CustomEvent('wtm-theme'));
      return;
    }
    if (event.source !== parent || event.data?.channel !== 'wtm.plugin.reply') return;
    const { id, result, error } = event.data;
    const request = pending.get(id);
    if (!request) return;
    pending.delete(id);
    clearTimeout(request.timer);
    if (error) request.reject(new Error(error));
    else request.resolve(result);
  });
  function request(method, params = {}) {
    if (pending.size >= 16) return Promise.reject(new Error('Too many pending requests'));
    const id = ++sequence;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        pending.delete(id);
        reject(new Error('Host request timed out; its outcome may be unknown'));
      }, 35000);
      pending.set(id, { resolve, reject, timer });
      parent.postMessage({ channel: 'wtm.plugin.request', id, method, params }, '*');
    });
  }
  globalThis.wtm = Object.freeze({ api: 2, request });
})();

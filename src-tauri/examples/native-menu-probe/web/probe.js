// This heartbeat is deliberately confined to the disposable native regression probe.
const { invoke } = window.__TAURI__.core;
const { Menu } = window.__TAURI__.menu;
const results = document.getElementById('results');
let last;
let busy = false;
function report(message) {
  results.textContent += `${message}\n`;
  void invoke('probe_report', { message });
}
async function stress(operation) {
  if (busy) return;
  busy = true;
  let active = true;
  report('Opened native UI. Wait five seconds before dismissing.');
  setTimeout(async () => {
    try {
      const resource = await Menu.new({ items: [{ text: 'Nested IPC resource' }] });
      await resource.close();
      const heartbeat = await invoke('probe_heartbeat');
      report(`${active ? 'PASS' : 'FAIL: dismissed too early'}: resource IPC + ${heartbeat}`);
    } catch (error) {
      report(`FAIL: ${JSON.stringify(error)}`);
    }
  }, 1000);
  try {
    await operation();
    report('Native UI returned.');
  } catch (error) {
    report(`FAIL: ${JSON.stringify(error)}`);
  } finally {
    active = false;
    busy = false;
  }
}
function picker(directory) {
  return stress(() => invoke('plugin:dialog|open', { options: { directory } }));
}
async function menu(old) {
  if (busy) return;
  await last?.close();
  let pendingPicker = false;
  let tracking = true;
  last = await Menu.new({ items: [
    { text: 'Open file picker', action: () => {
      if (tracking) pendingPicker = true;
      else void picker(false);
    } },
    { text: 'Dismiss', action: () => report('Menu action delivered.') },
  ] });
  await stress(() => old ? last.popup() : invoke('popup_native_menu', {
    rid: last.rid, kind: 'menu', at: null,
  }));
  tracking = false;
  if (pendingPicker) await picker(false);
}
document.getElementById('fixed').onclick = () => void menu(false);
document.getElementById('old').onclick = () => void menu(true);
document.getElementById('file').onclick = () => void picker(false);
document.getElementById('folder').onclick = () => void picker(true);

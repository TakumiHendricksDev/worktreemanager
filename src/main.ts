import { mount } from 'svelte';

import App from './App.svelte';

// xterm's own stylesheet, loaded before ours.
//
// The order matters and is the reason this is an import rather than the `<style>` element
// Terminal.svelte used to inject into `document.head` at mount. Injecting at mount put it
// *after* the bundle, so it won every specificity tie with app CSS. That was invisible while
// every component's styles were Svelte-scoped and therefore always more specific; with a
// global stylesheet both are single classes, and which one wins would have depended on
// whether a terminal had been opened yet.
//
// Vite bundles it as a local asset, so this satisfies the CSP for the same reason the
// injected version did — nothing is fetched from a remote origin.
import '@xterm/xterm/css/xterm.css';
import './styles/main.scss';

const target = document.getElementById('app');
if (!target) throw new Error('#app is missing from index.html');

export default mount(App, { target });

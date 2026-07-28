import { vitePreprocess } from '@sveltejs/vite-plugin-svelte';

export default {
  preprocess: vitePreprocess(),
  compilerOptions: {
    // Runes explicitly on, so a component that forgets `$state` is a compile error
    // rather than silently falling back to legacy reactivity.
    runes: true,
  },
};

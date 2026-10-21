// Plain Svelte (non-SvelteKit) config for the multi-page Vite setup.
// Each Tauri window loads its own HTML entry that mounts its own Svelte root.
import { vitePreprocess } from "@sveltejs/vite-plugin-svelte";

/** @type {import('@sveltejs/vite-plugin-svelte').Options['preprocess']} */
const config = {
  preprocess: vitePreprocess(),
};

export default config;

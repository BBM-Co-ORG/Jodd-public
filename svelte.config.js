import { vitePreprocess } from '@sveltejs/vite-plugin-svelte';
 
// Under Vitest the style pass is skipped: it hands `<style>` blocks to Vite's
// CSS pipeline, which needs a fully resolved Vite config that Vitest's
// transform pipeline never supplies — it throws "Cannot create proxy with a
// non-object as target or handler" before any component can be compiled.
// Component styles are irrelevant to the assertions we make in tests, and the
// dev/build path is unchanged. Script preprocessing is off in both cases
// (Svelte 5's compiler strips TypeScript itself).
const config = {
  preprocess: vitePreprocess(process.env.VITEST ? { style: false } : undefined),
};
 
export default config;

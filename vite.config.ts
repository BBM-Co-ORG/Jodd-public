import { defineConfig, configDefaults } from "vitest/config";
import { svelte } from "@sveltejs/vite-plugin-svelte";

export default defineConfig({
  plugins: [svelte()],
  test: {
    // Claude Code's worktrees live at .claude/worktrees/<name>/, i.e. INSIDE
    // the repo, each a full checkout carrying its own copy of every test.
    // Vitest's default exclude covers node_modules and dist but knows nothing
    // about them, so a plain `vitest run` collected the suite once per
    // worktree — 116 files / 2159 tests instead of 15 / 315, most of it
    // asserting against branches that are not the one being tested. A green
    // run then says nothing about the checkout you are in.
    exclude: [...configDefaults.exclude, ".claude/worktrees/**"],
  },
  // Vitest resolves packages with node conditions by default, which picks
  // Svelte's server build — `mount()` throws `lifecycle_function_unavailable`
  // there. Component tests need the client build; jsdom supplies the DOM.
  // Scoped to VITEST so the dev server and `tauri build` are untouched.
  resolve: process.env.VITEST ? { conditions: ['browser'] } : undefined,
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    watch: {
      // **`target/` is at the REPO ROOT, not under src-tauri — gotcha #5.**
      // This list said only `**/src-tauri/**`, which was right when the crate
      // was standalone and its build output lived inside it. Since the
      // workspace move both members build to repo-root `target/`, so the
      // pattern named a directory holding no artifacts and Vite watched the
      // whole tree: thousands of files, and on Windows the dev server dies at
      // startup with `EBUSY: resource busy or locked, watch
      // 'target\debug\deps\jodd.exe'` the moment one of them is a linked
      // binary. Invisible until then, because watching junk only wastes
      // resources.
      ignored: ["**/src-tauri/**", "**/target/**"],
    },
  },
});

#!/usr/bin/env node
// Regenerate ONLY the Android launcher icons, from
// `src-tauri/icons/android-icon.json`.
//
//   node scripts/gen-android-icons.mjs
//
// Three things this exists to get right, none of which `npx tauri icon` does
// on its own:
//
//  1. `tauri icon` always regenerates every platform from the manifest's
//     `default`, which would rewrite icon.icns / icon.ico / the iOS set as a
//     side effect of an Android-only change. So it is pointed at a scratch
//     directory and only the `android/` subtree is copied out.
//
//  2. The Android assets are committed TWICE — `src-tauri/icons/android` is
//     what the tool writes, `src-tauri/gen/android/app/src/main/res` is what
//     the APK ships — and only the second one reaches a user's launcher.
//     Updating one and not the other looks like a fixed icon in the diff and
//     ships the old one. Both are written here, from the same output.
//
//  3. `android_fg_scale` is not the inset knob it appears to be. Measured on
//     tauri-cli 2.11.2: the manifest field is parsed (a non-numeric value is
//     a hard error) but has no effect — scales 10, 50, 60 and 100 produce
//     byte-identical foregrounds, for PNG and SVG sources alike, with and
//     without `android_bg`. Android's safe-zone inset is therefore baked into
//     `source-android-foreground.svg` itself, and
//     `scripts/android-adaptive-icon.test.mjs` is what holds it there.

import { execFileSync } from "node:child_process";
import { cpSync, mkdtempSync, readdirSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const repoRoot = fileURLToPath(new URL("..", import.meta.url));
const manifest = "src-tauri/icons/android-icon.json";
const destinations = [
  "src-tauri/icons/android",
  "src-tauri/gen/android/app/src/main/res",
];

const scratch = mkdtempSync(join(tmpdir(), "jodd-android-icons-"));
try {
  // npx is a shell script on POSIX and a .cmd shim on Windows; execFileSync
  // does not consult PATHEXT, so name the shim explicitly rather than relying
  // on a shell.
  const npx = process.platform === "win32" ? "npx.cmd" : "npx";
  execFileSync(npx, ["tauri", "icon", manifest, "-o", scratch], {
    cwd: repoRoot,
    stdio: "inherit",
  });

  // Only the mipmap trees: the scratch directory also holds the desktop and
  // iOS icons this run regenerated and we are throwing away, and the res/
  // destination holds hand-maintained files (layout/, values/, xml/) that
  // `tauri icon` knows nothing about.
  const generated = join(scratch, "android");
  const mipmaps = readdirSync(generated).filter((e) => e.startsWith("mipmap-"));
  for (const destination of destinations) {
    for (const mipmap of mipmaps) {
      cpSync(join(generated, mipmap), join(repoRoot, destination, mipmap), {
        recursive: true,
      });
    }
    console.log(`updated ${destination}: ${mipmaps.sort().join(", ")}`);
  }
} finally {
  rmSync(scratch, { recursive: true, force: true });
}

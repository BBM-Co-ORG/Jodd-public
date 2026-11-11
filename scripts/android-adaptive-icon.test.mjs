import { describe, it, expect } from "vitest";
import { readdirSync, existsSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { readPngRgba } from "./read-png.mjs";

// Android adaptive icons hand the launcher two 108dp layers and let it decide
// what to show of them:
//
//   * the outer 18dp on every side is reserve — cropped away for the parallax
//     and bleed the launcher animates, so nothing there is ever guaranteed;
//   * the remaining centre 72dp is the viewport, and a launcher-chosen mask
//     (circle, squircle, rounded square, teardrop, …) is applied inside it;
//   * every mask is guaranteed to contain the centred 66dp circle, and that
//     circle is the only region a design may rely on.
//
// The BACKGROUND layer is meant to be full-bleed and is expected to lose its
// corners. The FOREGROUND layer carries the mark, so anything of it outside
// the safe circle is art the user may simply never see.
const LAYER_DP = 108;
const SAFE_CIRCLE_DP = 66;

const repoRoot = fileURLToPath(new URL("..", import.meta.url));

// Both trees are committed and both matter: `icons/android` is what
// `tauri icon` writes, `gen/android/…/res` is what the APK actually ships.
// Checking each of them separately is also what catches the two drifting.
const TREES = [
  "src-tauri/icons/android",
  "src-tauri/gen/android/app/src/main/res",
];

function foregrounds(tree) {
  const dir = join(repoRoot, tree);
  return readdirSync(dir)
    .filter((entry) => entry.startsWith("mipmap-"))
    .map((entry) => join(tree, entry, "ic_launcher_foreground.png"))
    .filter((rel) => existsSync(join(repoRoot, rel)))
    .sort();
}

/**
 * How far the furthest non-transparent pixel sits from the layer's centre,
 * measured in dp on the 108dp layer so it can be compared with Android's own
 * numbers regardless of which density bucket the file came from.
 */
function artworkRadiusDp(rel) {
  const { width, height, data } = readPngRgba(join(repoRoot, rel));
  const cx = width / 2;
  const cy = height / 2;
  let worst = 0;
  for (let y = 0; y < height; y++) {
    for (let x = 0; x < width; x++) {
      if (data[(y * width + x) * 4 + 3] === 0) continue;
      // Measure to the far edge of the pixel, not its origin.
      const dx = Math.max(Math.abs(x - cx), Math.abs(x + 1 - cx));
      const dy = Math.max(Math.abs(y - cy), Math.abs(y + 1 - cy));
      worst = Math.max(worst, Math.hypot(dx, dy));
    }
  }
  return (worst / width) * LAYER_DP;
}

describe("the Android adaptive launcher icon", () => {
  const files = TREES.flatMap(foregrounds);

  it("has a foreground in every density bucket of both trees", () => {
    expect(files.length).toBe(10);
  });

  it.each(files)("keeps %s inside the 66dp safe circle", (rel) => {
    const radiusDp = artworkRadiusDp(rel);
    // A launcher mask clips whatever reaches past this; the fractional slack
    // is antialiasing on the artwork's own edge, not a relaxation of the rule.
    expect(radiusDp).toBeLessThanOrEqual(SAFE_CIRCLE_DP / 2 + 0.25);
  });
});

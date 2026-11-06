// Minimal PNG reader for the icon guard tests.
//
// Node ships zlib but no image decoder, and the only thing that needs decoding
// here is a handful of committed launcher PNGs — all 8-bit RGBA, non-interlaced,
// straight out of `tauri icon`. Adding an image dependency to check four
// numbers per file is a worse trade than the fifty lines below, which refuse
// anything outside that shape rather than guessing at it.

import { readFileSync } from "node:fs";
import { inflateSync } from "node:zlib";

const SIGNATURE = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);

function paeth(a, b, c) {
  const p = a + b - c;
  const pa = Math.abs(p - a);
  const pb = Math.abs(p - b);
  const pc = Math.abs(p - c);
  if (pa <= pb && pa <= pc) return a;
  if (pb <= pc) return b;
  return c;
}

/**
 * Decode an 8-bit RGBA, non-interlaced PNG.
 *
 * @param {string} path
 * @returns {{ width: number, height: number, data: Uint8Array }} `data` is
 *   RGBA, 4 bytes per pixel, row-major.
 */
export function readPngRgba(path) {
  const buf = readFileSync(path);
  if (!buf.subarray(0, 8).equals(SIGNATURE)) {
    throw new Error(`${path}: not a PNG`);
  }

  let width = 0;
  let height = 0;
  const idat = [];
  for (let off = 8; off + 8 <= buf.length; ) {
    const length = buf.readUInt32BE(off);
    const type = buf.toString("ascii", off + 4, off + 8);
    const body = buf.subarray(off + 8, off + 8 + length);
    off += 12 + length; // length + type + data + CRC

    if (type === "IHDR") {
      width = body.readUInt32BE(0);
      height = body.readUInt32BE(4);
      const [bitDepth, colorType, , , interlace] = body.subarray(8, 13);
      if (bitDepth !== 8 || colorType !== 6 || interlace !== 0) {
        throw new Error(
          `${path}: expected 8-bit RGBA non-interlaced, got bitDepth=${bitDepth} colorType=${colorType} interlace=${interlace}`,
        );
      }
    } else if (type === "IDAT") {
      idat.push(body);
    } else if (type === "IEND") {
      break;
    }
  }
  if (!width || !height) throw new Error(`${path}: no IHDR`);

  const raw = inflateSync(Buffer.concat(idat));
  const bpp = 4;
  const stride = width * bpp;
  const data = new Uint8Array(width * height * bpp);

  // Undo the per-scanline filter. Each row is prefixed by its filter byte and
  // refers back to the row above, so this has to run in order.
  for (let y = 0; y < height; y++) {
    const filter = raw[y * (stride + 1)];
    const src = (y * (stride + 1)) + 1;
    const dst = y * stride;
    for (let x = 0; x < stride; x++) {
      const value = raw[src + x];
      const left = x >= bpp ? data[dst + x - bpp] : 0;
      const up = y > 0 ? data[dst - stride + x] : 0;
      const upLeft = y > 0 && x >= bpp ? data[dst - stride + x - bpp] : 0;
      let out;
      switch (filter) {
        case 0: out = value; break;
        case 1: out = value + left; break;
        case 2: out = value + up; break;
        case 3: out = value + ((left + up) >> 1); break;
        case 4: out = value + paeth(left, up, upLeft); break;
        default: throw new Error(`${path}: unknown row filter ${filter}`);
      }
      data[dst + x] = out & 0xff;
    }
  }

  return { width, height, data };
}

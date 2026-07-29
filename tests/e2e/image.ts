// Pixel-level comparison for the screenshot tests: decode the engine's PNG, sample a region, and
// score two frames against each other.
//
// Tests that only need "did anything change" compare the encoded bytes with `Buffer.equals`. That
// cannot answer "did *this part* change", which is what a claim about one shaded region needs —
// a whole-frame difference is expected whenever any surface in view changes for any reason.
//
// The decoder is deliberately narrow: it reads exactly what `PngEncoder::write_image(…, Rgb8)`
// emits — 8-bit RGB, colour type 2, non-interlaced. Anything else is rejected rather than guessed
// at, so a format change surfaces as a decode error instead of silently wrong pixels.

import { inflateSync } from "node:zlib";

/// A decoded 8-bit RGB image. `pixels` is tightly packed, three bytes per pixel, row-major.
export interface Rgb8Image {
  width: number;
  height: number;
  pixels: Buffer;
}

/// A rectangle in pixels, clamped to the image when sampled.
export interface Region {
  x: number;
  y: number;
  width: number;
  height: number;
}

/// Decodes an 8-bit RGB, non-interlaced PNG.
export function decodeRgb8Png(png: Buffer): Rgb8Image {
  const signature = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
  if (!png.subarray(0, 8).equals(signature)) {
    throw new Error("not a PNG (bad signature)");
  }
  let width = 0;
  let height = 0;
  const idat: Buffer[] = [];
  let offset = 8;
  while (offset + 8 <= png.length) {
    const length = png.readUInt32BE(offset);
    const type = png.toString("ascii", offset + 4, offset + 8);
    const body = png.subarray(offset + 8, offset + 8 + length);
    if (type === "IHDR") {
      width = body.readUInt32BE(0);
      height = body.readUInt32BE(4);
      const depth = body.readUInt8(8);
      const colorType = body.readUInt8(9);
      const interlace = body.readUInt8(12);
      if (depth !== 8 || colorType !== 2 || interlace !== 0) {
        throw new Error(
          `unsupported PNG (depth ${depth}, colorType ${colorType}, interlace ${interlace}); ` +
            "this decoder reads only the engine's 8-bit non-interlaced RGB output",
        );
      }
    } else if (type === "IDAT") {
      idat.push(body);
    } else if (type === "IEND") {
      break;
    }
    offset += 12 + length; // length + type + body + crc
  }
  if (width === 0 || height === 0 || idat.length === 0) {
    throw new Error("PNG carried no image data");
  }
  return { width, height, pixels: unfilter(inflateSync(Buffer.concat(idat)), width, height) };
}

/// Reverses the per-scanline PNG filters into tightly-packed RGB rows.
function unfilter(raw: Buffer, width: number, height: number): Buffer {
  const bpp = 3;
  const stride = width * bpp;
  const out = Buffer.alloc(stride * height);
  let src = 0;
  for (let y = 0; y < height; y += 1) {
    const filter = raw.readUInt8(src);
    src += 1;
    const row = y * stride;
    const prior = row - stride;
    for (let i = 0; i < stride; i += 1) {
      const x = raw.readUInt8(src + i);
      const a = i >= bpp ? out[row + i - bpp]! : 0;
      const b = y > 0 ? out[prior + i]! : 0;
      const c = y > 0 && i >= bpp ? out[prior + i - bpp]! : 0;
      let value: number;
      switch (filter) {
        case 0:
          value = x;
          break;
        case 1:
          value = x + a;
          break;
        case 2:
          value = x + b;
          break;
        case 3:
          value = x + ((a + b) >> 1);
          break;
        case 4:
          value = x + paeth(a, b, c);
          break;
        default:
          throw new Error(`unknown PNG filter ${filter} on row ${y}`);
      }
      out[row + i] = value & 0xff;
    }
    src += stride;
  }
  return out;
}

function paeth(a: number, b: number, c: number): number {
  const p = a + b - c;
  const pa = Math.abs(p - a);
  const pb = Math.abs(p - b);
  const pc = Math.abs(p - c);
  if (pa <= pb && pa <= pc) {
    return a;
  }
  return pb <= pc ? b : c;
}

/// The mean channel value over `region`, in `[0, 255]` — the region's brightness. A shadow falling
/// on a surface lowers it; the surface being lit raises it.
export function regionMean(image: Rgb8Image, region: Region): number {
  const x0 = Math.max(0, region.x);
  const y0 = Math.max(0, region.y);
  const x1 = Math.min(image.width, region.x + region.width);
  const y1 = Math.min(image.height, region.y + region.height);
  if (x1 <= x0 || y1 <= y0) {
    throw new Error(`region ${JSON.stringify(region)} lies outside the image`);
  }
  let total = 0;
  let count = 0;
  for (let y = y0; y < y1; y += 1) {
    for (let x = x0; x < x1; x += 1) {
      const i = (y * image.width + x) * 3;
      total += image.pixels[i]! + image.pixels[i + 1]! + image.pixels[i + 2]!;
      count += 3;
    }
  }
  return total / count;
}

/// Mean absolute per-channel difference between two same-sized images, in `[0, 255]`. The metric a
/// cross-platform comparison scores: identical renders are 0, and a tolerance admits the
/// quantization and rounding two implementations are allowed to disagree on.
export function meanAbsoluteDifference(a: Rgb8Image, b: Rgb8Image): number {
  if (a.width !== b.width || a.height !== b.height) {
    throw new Error(
      `image sizes differ: ${a.width}x${a.height} vs ${b.width}x${b.height}`,
    );
  }
  let total = 0;
  for (let i = 0; i < a.pixels.length; i += 1) {
    total += Math.abs(a.pixels[i]! - b.pixels[i]!);
  }
  return total / a.pixels.length;
}

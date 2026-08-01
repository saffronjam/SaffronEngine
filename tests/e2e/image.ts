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

import { deflateSync, inflateSync } from "node:zlib";

// A decoded 8-bit RGB image. `pixels` is tightly packed, three bytes per pixel, row-major.
export interface Rgb8Image {
  width: number;
  height: number;
  pixels: Buffer;
}

// A rectangle in pixels, clamped to the image when sampled.
export interface Region {
  x: number;
  y: number;
  width: number;
  height: number;
}

// Decodes an 8-bit RGB, non-interlaced PNG.
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

// Reverses the per-scanline PNG filters into tightly-packed RGB rows.
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

// The mean channel value over `region`, in `[0, 255]` — the region's brightness. A shadow falling
// on a surface lowers it; the surface being lit raises it.
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

// Writes an 8-bit RGBA PNG the engine's importer accepts, `pixel(x, y)` supplying each texel.
//
// Authored textures are generated rather than checked in so the property a fixture depends on — a
// cutout that straddles the alpha cutoff, a height field with real gradient — is visible as intent
// instead of opaque bytes.
export function encodeRgba8Png(
  width: number,
  height: number,
  pixel: (x: number, y: number) => [number, number, number, number],
): Buffer {
  const raw: number[] = [];
  for (let y = 0; y < height; y += 1) {
    raw.push(0); // PNG filter byte: none
    for (let x = 0; x < width; x += 1) {
      raw.push(...pixel(x, y));
    }
  }
  const crcTable = Array.from({ length: 256 }, (_, n) => {
    let c = n;
    for (let k = 0; k < 8; k += 1) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    return c >>> 0;
  });
  const crc = (buf: Buffer) => {
    let c = 0xffffffff;
    for (const byte of buf) c = crcTable[(c ^ byte) & 0xff]! ^ (c >>> 8);
    return (c ^ 0xffffffff) >>> 0;
  };
  const chunk = (type: string, data: Buffer) => {
    const head = Buffer.alloc(4);
    head.writeUInt32BE(data.length);
    const body = Buffer.concat([Buffer.from(type, "ascii"), data]);
    const tail = Buffer.alloc(4);
    tail.writeUInt32BE(crc(body));
    return Buffer.concat([head, body, tail]);
  };
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(width, 0);
  ihdr.writeUInt32BE(height, 4);
  ihdr[8] = 8; // bit depth
  ihdr[9] = 6; // colour type: RGBA
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk("IHDR", ihdr),
    // PNG IDAT carries a zlib stream, not raw deflate — `node:zlib` wraps it, Bun's does not.
    chunk("IDAT", deflateSync(Buffer.from(raw))),
    chunk("IEND", Buffer.alloc(0)),
  ]);
}

// The per-channel absolute difference of two same-sized images, as an image. Isolating one term of
// the shading — a frame with a feature against the same frame without it — leaves an image every
// other term has cancelled out of, which two such isolations can then be compared across.
export function absoluteDifferenceImage(a: Rgb8Image, b: Rgb8Image): Rgb8Image {
  if (a.width !== b.width || a.height !== b.height) {
    throw new Error(`image sizes differ: ${a.width}x${a.height} vs ${b.width}x${b.height}`);
  }
  const pixels = Buffer.allocUnsafe(a.pixels.length);
  for (let i = 0; i < a.pixels.length; i += 1) {
    pixels[i] = Math.abs(a.pixels[i]! - b.pixels[i]!);
  }
  return { width: a.width, height: a.height, pixels };
}

// Mean absolute per-channel difference between two same-sized images, in `[0, 255]`. The metric a
// cross-platform comparison scores: identical renders are 0, and a tolerance admits the
// quantization and rounding two implementations are allowed to disagree on.
export function meanAbsoluteDifference(a: Rgb8Image, b: Rgb8Image): number {
  if (a.width !== b.width || a.height !== b.height) {
    throw new Error(`image sizes differ: ${a.width}x${a.height} vs ${b.width}x${b.height}`);
  }
  let total = 0;
  for (let i = 0; i < a.pixels.length; i += 1) {
    total += Math.abs(a.pixels[i]! - b.pixels[i]!);
  }
  return total / a.pixels.length;
}

// The greatest single-channel difference between two same-sized images, in `[0, 255]`.
//
// A mean answers "how much of the frame differs"; this answers "did anything differ *visibly*".
// The two part company on a frame carrying denoiser residue: a pass that resolves across frames
// keeps dithering the pixels it covers by one 8-bit level indefinitely, which a mean pools into a
// number that grows with the area covered, while the peak reads it for what it is — a single
// quantization step, no silhouette moved.
export function peakAbsoluteDifference(a: Rgb8Image, b: Rgb8Image): number {
  if (a.width !== b.width || a.height !== b.height) {
    throw new Error(`image sizes differ: ${a.width}x${a.height} vs ${b.width}x${b.height}`);
  }
  let peak = 0;
  for (let i = 0; i < a.pixels.length; i += 1) {
    const difference = Math.abs(a.pixels[i]! - b.pixels[i]!);
    if (difference > peak) {
      peak = difference;
    }
  }
  return peak;
}

// How many channels of two same-sized images differ by at least `step` levels. Paired with a
// `step` above the frame's residue, this is a count of what actually moved rather than a score
// of how much everything drifted.
export function channelsDifferingBy(a: Rgb8Image, b: Rgb8Image, step: number): number {
  if (a.width !== b.width || a.height !== b.height) {
    throw new Error(`image sizes differ: ${a.width}x${a.height} vs ${b.width}x${b.height}`);
  }
  let count = 0;
  for (let i = 0; i < a.pixels.length; i += 1) {
    if (Math.abs(a.pixels[i]! - b.pixels[i]!) >= step) {
      count += 1;
    }
  }
  return count;
}

// Generates Self-KVM app icons as PNGs with zero dependencies (manual PNG
// encoding via Node's zlib). Motif: two stacked "screens" linked by a signal
// line — a KVM switch — on a deep control-deck gradient.
import { deflateSync } from "node:zlib";
import { writeFileSync } from "node:fs";

const CRC = (() => {
  const t = new Uint32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    t[n] = c >>> 0;
  }
  return (buf) => {
    let c = 0xffffffff;
    for (let i = 0; i < buf.length; i++) c = t[(c ^ buf[i]) & 0xff] ^ (c >>> 8);
    return (c ^ 0xffffffff) >>> 0;
  };
})();

function chunk(type, data) {
  const len = Buffer.alloc(4);
  len.writeUInt32BE(data.length, 0);
  const td = Buffer.concat([Buffer.from(type, "ascii"), data]);
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(CRC(td), 0);
  return Buffer.concat([len, td, crc]);
}

function png(size) {
  const px = new Uint8Array(size * size * 4);
  const set = (x, y, r, g, b, a = 255) => {
    x = Math.round(x);
    y = Math.round(y);
    if (x < 0 || y < 0 || x >= size || y >= size) return;
    const i = (y * size + x) * 4;
    const af = a / 255;
    px[i] = px[i] * (1 - af) + r * af;
    px[i + 1] = px[i + 1] * (1 - af) + g * af;
    px[i + 2] = px[i + 2] * (1 - af) + b * af;
    px[i + 3] = Math.max(px[i + 3], a);
  };

  const R = size * 0.18; // corner radius of the app tile
  const inRound = (x, y, x0, y0, x1, y1, r) => {
    const cx = Math.min(Math.max(x, x0 + r), x1 - r);
    const cy = Math.min(Math.max(y, y0 + r), y1 - r);
    const within = x >= x0 && x <= x1 && y >= y0 && y <= y1;
    if (!within) return false;
    const dx = x - cx, dy = y - cy;
    return dx * dx + dy * dy <= r * r ||
      (x > x0 + r && x < x1 - r) || (y > y0 + r && y < y1 - r);
  };

  for (let y = 0; y < size; y++) {
    for (let x = 0; x < size; x++) {
      if (!inRound(x, y, 0, 0, size - 1, size - 1, R)) continue;
      // diagonal gradient: deep navy -> teal
      const t = (x + y) / (2 * size);
      const r = 9 + t * 6;
      const g = 16 + t * 40;
      const b = 32 + t * 38;
      set(x, y, r, g, b, 255);
    }
  }

  // Two screens (rounded rects) and a connecting signal line.
  const cyan = [34, 211, 238];
  const amber = [251, 191, 36];
  const sw = size * 0.42, sh = size * 0.20, sr = size * 0.04;
  const cx = size / 2;
  const top = { x: cx - sw / 2, y: size * 0.22 };
  const bot = { x: cx - sw / 2, y: size * 0.58 };

  const stroke = (rx, ry, col) => {
    const x0 = rx, y0 = ry, x1 = rx + sw, y1 = ry + sh;
    const th = Math.max(2, size * 0.018);
    for (let y = y0 - th; y <= y1 + th; y++) {
      for (let x = x0 - th; x <= x1 + th; x++) {
        const inOuter = inRound(x, y, x0 - th, y0 - th, x1 + th, y1 + th, sr + th);
        const inInner = inRound(x, y, x0, y0, x1, y1, sr);
        if (inOuter && !inInner) set(x, y, col[0], col[1], col[2], 255);
        else if (inInner) set(x, y, col[0], col[1], col[2], 38); // faint fill
      }
    }
  };
  stroke(top.x, top.y, cyan);
  stroke(bot.x, bot.y, cyan);

  // signal line + node between the screens
  const lineX = cx;
  const th = Math.max(2, size * 0.02);
  for (let y = top.y + sh; y <= bot.y; y++) {
    for (let dx = -th; dx <= th; dx++) set(lineX + dx, y, amber[0], amber[1], amber[2], 255);
  }
  const nodeR = size * 0.045;
  for (let y = -nodeR; y <= nodeR; y++)
    for (let x = -nodeR; x <= nodeR; x++)
      if (x * x + y * y <= nodeR * nodeR)
        set(Math.round(lineX + x), Math.round((top.y + sh + bot.y) / 2 + y), amber[0], amber[1], amber[2], 255);

  // PNG assembly
  const raw = Buffer.alloc((size * 4 + 1) * size);
  for (let y = 0; y < size; y++) {
    raw[y * (size * 4 + 1)] = 0; // filter: none
    for (let x = 0; x < size * 4; x++) raw[y * (size * 4 + 1) + 1 + x] = px[y * size * 4 + x];
  }
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(size, 0);
  ihdr.writeUInt32BE(size, 4);
  ihdr[8] = 8; // bit depth
  ihdr[9] = 6; // RGBA
  const sig = Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]);
  return Buffer.concat([
    sig,
    chunk("IHDR", ihdr),
    chunk("IDAT", deflateSync(raw, { level: 9 })),
    chunk("IEND", Buffer.alloc(0)),
  ]);
}

const dir = new URL(".", import.meta.url).pathname;
for (const [name, size] of [
  ["32x32.png", 32],
  ["128x128.png", 128],
  ["128x128@2x.png", 256],
  ["icon.png", 512],
]) {
  writeFileSync(dir + name, png(size));
  console.log("wrote", name, size + "px");
}

// Draws Afterclip's mark from one set of shapes: a rewind ring around a stylised A. It writes the vector
// logo and the PNGs the app icon is made from, so every size stays the same drawing.
//   node scripts/build-brand.js
const fs = require('node:fs');
const path = require('node:path');
const zlib = require('node:zlib');

const BRAND = [0x7c, 0x6c, 0xff];
const INK = [0x12, 0x0f, 0x2e];
const WHITE = [0xff, 0xff, 0xff];
const TRAY_TILE = [0x5b, 0x49, 0xe6];
const hex = (c) => `#${c.map((v) => v.toString(16).padStart(2, '0')).join('')}`;

// All in a unit square. Screen angles: 0 is right and -90 is up.
const C = 0.5;
const RING_R = 0.3;
const RING_W = 0.074;
const GAP_FROM = -148; // The ring runs clockwise from the arrow's base round to here.
const HEAD_AT = -78; // Where the arrowhead's base sits. It points counterclockwise, into the gap: rewinding.
const HEAD_SPREAD = 0.082; // Half the base width, across the ring.
const HEAD_LEN = 30; // How far round the ring the tip reaches, in degrees.
const TILE_CORNER = 0.22;
// How far the mark fills a space with no tile. Any larger and the arrowhead's tip gets cut off.
const MARK_ZOOM = 1.25;
const A_TOP = [0.5, 0.345];
const A_LEFT = [0.392, 0.635];
const A_RIGHT = [0.608, 0.635];
const NOTCH_TOP = [0.5, 0.545];
const NOTCH_LEFT = [0.455, 0.635];
const NOTCH_RIGHT = [0.545, 0.635];

const polar = (deg, r) => [C + r * Math.cos((deg * Math.PI) / 180), C + r * Math.sin((deg * Math.PI) / 180)];
const HEAD = [polar(HEAD_AT, RING_R + HEAD_SPREAD), polar(HEAD_AT, RING_R - HEAD_SPREAD), polar(HEAD_AT - HEAD_LEN, RING_R)];

function inTriangle(p, a, b, c) {
  const side = (u, v) => (p[0] - v[0]) * (u[1] - v[1]) - (u[0] - v[0]) * (p[1] - v[1]);
  const d1 = side(a, b); const d2 = side(b, c); const d3 = side(c, a);
  return !((d1 < 0 || d2 < 0 || d3 < 0) && (d1 > 0 || d2 > 0 || d3 > 0));
}

function inMark(x, y) {
  const r = Math.hypot(x - C, y - C);
  if (Math.abs(r - RING_R) <= RING_W / 2) {
    let a = (Math.atan2(y - C, x - C) * 180) / Math.PI;
    if (a < HEAD_AT) a += 360;
    if (a <= GAP_FROM + 360) return true;
  }
  if (inTriangle([x, y], ...HEAD)) return true;
  return inTriangle([x, y], A_TOP, A_LEFT, A_RIGHT) && !inTriangle([x, y], NOTCH_TOP, NOTCH_LEFT, NOTCH_RIGHT);
}

function inTile(x, y) {
  const qx = Math.abs(x - 0.5) - (0.5 - TILE_CORNER);
  const qy = Math.abs(y - 0.5) - (0.5 - TILE_CORNER);
  return Math.hypot(Math.max(qx, 0), Math.max(qy, 0)) + Math.min(Math.max(qx, qy), 0) <= TILE_CORNER;
}

function chunk(type, data) {
  const body = Buffer.concat([Buffer.from(type), data]);
  const out = Buffer.alloc(body.length + 8);
  out.writeUInt32BE(data.length, 0);
  body.copy(out, 4);
  out.writeUInt32BE(zlib.crc32(body), body.length + 4);
  return out;
}

function encodePng(size, pixel) {
  const SAMPLES = 4;
  const raw = Buffer.alloc((size * 4 + 1) * size);
  for (let py = 0; py < size; py++) {
    for (let px = 0; px < size; px++) {
      let [r, g, b, a] = [0, 0, 0, 0];
      for (let sy = 0; sy < SAMPLES; sy++) {
        for (let sx = 0; sx < SAMPLES; sx++) {
          const [cr, cg, cb, ca] = pixel((px + (sx + 0.5) / SAMPLES) / size, (py + (sy + 0.5) / SAMPLES) / size);
          r += cr * ca; g += cg * ca; b += cb * ca; a += ca;
        }
      }
      const out = a ? [r / a, g / a, b / a, (a / (SAMPLES * SAMPLES)) * 255] : [0, 0, 0, 0];
      raw.set(out.map(Math.round), py * (size * 4 + 1) + 1 + px * 4);
    }
  }
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(size, 0);
  ihdr.writeUInt32BE(size, 4);
  ihdr.set([8, 6, 0, 0, 0], 8);
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk('IHDR', ihdr), chunk('IDAT', zlib.deflateSync(raw)), chunk('IEND', Buffer.alloc(0)),
  ]);
}

/** The app icon: the amber mark on a dark rounded tile. */
const tilePng = (size) => encodePng(size, (x, y) => {
  if (!inTile(x, y)) return [0, 0, 0, 0];
  return inMark(x, y) ? [...BRAND, 1] : [...INK, 1];
});

/**
 * Just the mark, filling the image, for small places like the tray. `zoom` crops the mark's empty margin,
 * because at 16 pixels the tile's margin would waste a third of the icon.
 */
const markPng = (size, colour, zoom = MARK_ZOOM) => encodePng(size, (x, y) => (
  inMark(C + (x - C) / zoom, C + (y - C) / zoom) ? [...colour, 1] : [0, 0, 0, 0]
));

/**
 * The tray has at most 32 pixels, and Windows draws a bare mark smaller than its neighbours. So it gets its own
 * bolder drawing: a tile that fills the whole space, with a thicker ring and a solid A, in white so it reads on
 * light and dark taskbars.
 */
const TRAY_RING_R = 0.345;
const TRAY_RING_W = 0.115;
const TRAY_CORNER = 0.22;
const TRAY_HEAD = [
  polar(HEAD_AT, TRAY_RING_R + 0.115), polar(HEAD_AT, TRAY_RING_R - 0.115), polar(HEAD_AT - 36, TRAY_RING_R),
];
const TRAY_A = [[0.5, 0.385], [0.375, 0.655], [0.625, 0.655]];

function inTrayMark(x, y) {
  const r = Math.hypot(x - C, y - C);
  if (Math.abs(r - TRAY_RING_R) <= TRAY_RING_W / 2) {
    let a = (Math.atan2(y - C, x - C) * 180) / Math.PI;
    if (a < HEAD_AT) a += 360;
    if (a <= GAP_FROM + 360) return true;
  }
  return inTriangle([x, y], ...TRAY_HEAD) || inTriangle([x, y], ...TRAY_A);
}

const trayPng = (size) => encodePng(size, (x, y) => {
  const qx = Math.abs(x - 0.5) - (0.5 - TRAY_CORNER);
  const qy = Math.abs(y - 0.5) - (0.5 - TRAY_CORNER);
  if (Math.hypot(Math.max(qx, 0), Math.max(qy, 0)) + Math.min(Math.max(qx, qy), 0) > TRAY_CORNER) return [0, 0, 0, 0];
  return inTrayMark(x, y) ? [...WHITE, 1] : [...TRAY_TILE, 1];
});

/** Just the rewind ring, as SVG for a 144px key, so a number can sit where the A does. `COLOUR` is filled in later. */
function ringForKey(zoom = MARK_ZOOM) {
  const n = (v) => +(v * 144).toFixed(2);
  const pt = ([x, y]) => `${n(x)} ${n(y)}`;
  const off = +(C * (1 - zoom) * 144).toFixed(2);
  const [a, b, c] = HEAD;
  return `<g transform="translate(${off} ${off}) scale(${zoom})"><path d="M${pt(polar(HEAD_AT, RING_R))}A${n(RING_R)} ${n(RING_R)} 0 1 1 ${pt(polar(GAP_FROM, RING_R))}" fill="none" stroke="COLOUR" stroke-width="${n(RING_W)}"/><path d="M${pt(a)}L${pt(b)}L${pt(c)}Z" fill="COLOUR"/></g>`;
}

function svg({ tile, colour = BRAND, zoom = 1 }) {
  const n = (v) => +(v * 1000).toFixed(1);
  const pt = ([x, y]) => `${n(x)} ${n(y)}`;
  const tri = (a, b, c) => `M${pt(a)}L${pt(b)}L${pt(c)}Z`;
  const from = polar(HEAD_AT, RING_R);
  const to = polar(GAP_FROM, RING_R);
  const off = C * (1 - zoom) * 1000;
  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1000 1000" role="img" aria-label="Afterclip">
${tile ? `<rect width="1000" height="1000" rx="${n(TILE_CORNER)}" fill="${hex(INK)}"/>\n` : ''}<g fill="${hex(colour)}" transform="translate(${off} ${off}) scale(${zoom})">
<path d="M${pt(from)}A${n(RING_R)} ${n(RING_R)} 0 1 1 ${pt(to)}" fill="none" stroke="${hex(colour)}" stroke-width="${n(RING_W)}"/>
<path d="${tri(...HEAD)}"/>
<path fill-rule="evenodd" d="${tri(A_TOP, A_LEFT, A_RIGHT)}${tri(NOTCH_TOP, NOTCH_RIGHT, NOTCH_LEFT)}"/>
</g>
</svg>
`;
}

module.exports = { tilePng, markPng, ringForKey, BRAND, WHITE };

if (require.main === module) {
  const root = path.join(__dirname, '..');
  const brand = path.join(root, 'branding');
  fs.mkdirSync(brand, { recursive: true });
  fs.writeFileSync(path.join(brand, 'afterclip-icon.svg'), svg({ tile: true }));
  fs.writeFileSync(path.join(brand, 'afterclip-mark.svg'), svg({ tile: false, zoom: MARK_ZOOM }));
  fs.writeFileSync(path.join(brand, 'afterclip-icon-1024.png'), tilePng(1024));
  fs.writeFileSync(path.join(root, 'src', 'renderer', 'logo.svg'), svg({ tile: false, zoom: MARK_ZOOM }));
  // One exact size per display scale, because Windows blurs any it has to resize.
  for (const size of [16, 20, 24, 32]) fs.writeFileSync(path.join(root, 'src-tauri', 'icons', `tray-${size}.png`), trayPng(size));
  fs.rmSync(path.join(root, 'src-tauri', 'icons', 'tray.png'), { force: true });
}

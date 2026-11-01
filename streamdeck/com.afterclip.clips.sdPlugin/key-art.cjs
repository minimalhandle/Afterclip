// Draws every key: Afterclip's rewind ring, with the number or symbol where the logo has its A.
// Used by the plugin for live keys, and by scripts/build-plugin.js for the images keys start with.
const RING = require('./ring.cjs');

const COLOURS = {
  ink: '#120f2e', panel: '#171433', ring: '#7c6cff', accent: '#5b49e6', text: '#ffffff', muted: '#9a95bd',
};

const text = (value, y, size, fill, weight = 700) => `<text x="72" y="${y}" text-anchor="middle" font-family="Segoe UI, sans-serif" font-weight="${weight}" font-size="${size}" fill="${fill}">${value}</text>`;

const SYMBOLS = {
  record: (fill) => `<circle cx="72" cy="66" r="19" fill="${fill}"/>`,
  stop: (fill) => `<rect x="55" y="49" width="34" height="34" rx="4" fill="${fill}"/>`,
  mic: (fill) => `<rect x="62" y="40" width="20" height="35" rx="10" fill="${fill}"/><path d="M53 63a19 19 0 0 0 38 0M72 82v9" fill="none" stroke="${fill}" stroke-width="5" stroke-linecap="round"/>`,
  clean: (fill) => `<path d="M72 36l7 21 21 7-21 7-7 21-7-21-21-7 21-7z" fill="${fill}"/><path d="M96 74l3 9 9 3-9 3-3 9-3-9-9-3 9-3z" fill="${fill}"/>`,
  marker: (fill) => `<path d="M58 92V42h34l-9 13 9 13H58" fill="${fill}" stroke="${fill}" stroke-width="5" stroke-linejoin="round" stroke-linecap="round"/>`,
  micMuted: (fill, background) => `${SYMBOLS.mic(fill)}<path d="M50 39l44 50" stroke="${background}" stroke-width="10"/><path d="M50 39l44 50" stroke="${fill}" stroke-width="5" stroke-linecap="round"/>`,
};

/**
 * face: { big, small } for text in the ring, or { symbol, small }. tone: normal, active (filled, for saving,
 * recording or muted), or off (dimmed, when the action isn't available).
 */
function keySvg({ big, small, symbol, tone = 'normal' }) {
  const background = tone === 'active' ? COLOURS.accent : tone === 'off' ? COLOURS.panel : COLOURS.ink;
  const ring = tone === 'active' ? COLOURS.text : tone === 'off' ? COLOURS.muted : COLOURS.ring;
  const main = tone === 'off' ? COLOURS.muted : COLOURS.text;
  const size = !big ? 0 : big.length > 3 ? 27 : big.length > 2 ? 34 : 46;
  const label = small ? text(small, symbol ? 104 : 98, small.length > 5 ? 12 : 15, main, 600) : '';
  const middle = symbol ? SYMBOLS[symbol](main, background) : text(big, small ? 79 : 88, size, main);
  return `<svg xmlns="http://www.w3.org/2000/svg" width="144" height="144" viewBox="0 0 144 144"><rect width="144" height="144" fill="${background}"/>${RING.replaceAll('COLOUR', ring)}${middle}${label}</svg>`;
}

/** The small white icon in Stream Deck's action list. */
function listIcon(label) {
  const size = label.length > 1 ? 46 : 58;
  return `<svg xmlns="http://www.w3.org/2000/svg" width="20" height="20" viewBox="0 0 144 144">${RING.replaceAll('COLOUR', '#ffffff')}${text(label, 92, size, '#ffffff')}</svg>`;
}

module.exports = { keySvg, listIcon, COLOURS };

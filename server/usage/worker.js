// Afterclip usage counter: a Cloudflare Worker with a D1 database, at afterclip.app/api/*.
// Copies that opted in send { id, version } once a day. It keeps one row per ID per day and nothing
// else: no IP addresses, no names, no other details.
//
// The dashboard at /api/stats also reads Sleekmeeter's totals, so it shows users across both apps.

import { statsPage } from './stats-page.js';

// Rows older than this are deleted every day.
const KEEP_DAYS = 400;
const SLEEKMEETER_STATS = 'https://sleekmeeter.app/api/stats?format=json';

const json = (body, status = 200, extra = {}) =>
  new Response(JSON.stringify(body), { status, headers: { 'content-type': 'application/json', ...extra } });

export function valid(p) {
  return !!p && typeof p === 'object'
    && typeof p.id === 'string' && /^[0-9a-f]{32}$/i.test(p.id)
    // 1.6.0, or a pre-release such as 1.7.0-beta.1.
    && typeof p.version === 'string' && /^\d{1,3}\.\d{1,3}\.\d{1,3}(-[0-9a-z.]{1,20})?$/i.test(p.version);
}

const today = () => new Date().toISOString().slice(0, 10);

async function ping(request, env) {
  if (request.method !== 'POST') return json({ error: 'POST only' }, 405);
  if (Number(request.headers.get('content-length') || 0) > 512) return json({ error: 'Too large' }, 413);
  let p;
  try { p = await request.json(); } catch { return json({ error: 'Not JSON' }, 400); }
  if (!valid(p)) return json({ error: 'Unexpected fields' }, 400);
  await env.DB.prepare('INSERT OR IGNORE INTO pings (day, id, version) VALUES (?1, ?2, ?3)')
    .bind(today(), p.id.toLowerCase(), p.version).run();
  return new Response(null, { status: 204 });
}

// Totals only, so anyone may read them. The same shape as Sleekmeeter's, without editions.
export async function readStats(env) {
  const one = async (sql) => (await env.DB.prepare(sql).first()) || {};
  const all = async (sql) => (await env.DB.prepare(sql).all()).results || [];
  return {
    today: (await one("SELECT COUNT(*) AS n FROM pings WHERE day = date('now')")).n || 0,
    last7Days: (await one("SELECT COUNT(DISTINCT id) AS n FROM pings WHERE day > date('now', '-7 days')")).n || 0,
    last30Days: (await one("SELECT COUNT(DISTINCT id) AS n FROM pings WHERE day > date('now', '-30 days')")).n || 0,
    daily: await all("SELECT day, COUNT(*) AS users FROM pings WHERE day > date('now', '-30 days') GROUP BY day ORDER BY day"),
    versions: await all("SELECT version, COUNT(DISTINCT id) AS users FROM pings WHERE day > date('now', '-30 days') GROUP BY version ORDER BY users DESC"),
  };
}

// Sleekmeeter's public totals, or null if they can't be read right now.
async function readSleekmeeter(env) {
  try {
    const request = new Request(SLEEKMEETER_STATS, { headers: { accept: 'application/json' } });
    const response = await (env.SLEEKMEETER ? env.SLEEKMEETER.fetch(request) : fetch(request, { cf: { cacheTtl: 60 } }));
    if (!response.ok) throw new Error(`HTTP ${response.status}`);
    const data = await response.json();
    return typeof data?.today === 'number' ? data : null;
  } catch (error) {
    console.error('Could not read Sleekmeeter usage totals', error);
    return null;
  }
}

// Both apps' totals. Each app has its own random IDs, so someone who uses both is counted once in each.
export function combine(afterclip, sleekmeeter) {
  const s = sleekmeeter || { today: 0, last7Days: 0, last30Days: 0, daily: [] };
  const days = new Map();
  for (const [app, rows] of [['afterclip', afterclip.daily], ['sleekmeeter', s.daily || []]]) {
    for (const { day, users } of rows) {
      const row = days.get(day) || { day, afterclip: 0, sleekmeeter: 0 };
      row[app] = users;
      days.set(day, row);
    }
  }
  return {
    today: afterclip.today + s.today,
    last7Days: afterclip.last7Days + s.last7Days,
    last30Days: afterclip.last30Days + s.last30Days,
    daily: [...days.values()].sort((a, b) => a.day.localeCompare(b.day)).map((row) => ({ ...row, users: row.afterclip + row.sleekmeeter })),
  };
}

export async function readAll(env) {
  const [afterclip, sleekmeeter] = await Promise.all([readStats(env), readSleekmeeter(env)]);
  return { total: combine(afterclip, sleekmeeter), afterclip, sleekmeeter };
}

function wantsStatsPage(request, url) {
  if (url.searchParams.has('format')) return false;
  const qualities = new Map();
  for (const entry of (request.headers.get('accept') || '').split(',')) {
    const [type, ...parameters] = entry.toLowerCase().split(';').map((part) => part.trim());
    const quality = parameters.find((part) => part.startsWith('q='));
    const value = quality === undefined ? 1 : Number(quality.slice(2));
    if (value >= 0 && value <= 1) qualities.set(type, Math.max(qualities.get(type) || 0, value));
  }
  return (qualities.get('text/html') || 0) > (qualities.get('application/json') || 0);
}

// Browsers get the dashboard for both apps. Scripts get Afterclip's own JSON, like Sleekmeeter's, or
// both apps with ?format=all.
async function stats(request, url, env) {
  const headers = { vary: 'Accept' };
  const format = url.searchParams.get('format');
  if (format === 'all') {
    return json(await readAll(env), 200, { ...headers, 'cache-control': 'private, max-age=60' });
  }
  if (!wantsStatsPage(request, url)) {
    // Shared caches can ignore Vary, so let each client cache the JSON separately.
    return json(await readStats(env), 200, { ...headers, 'cache-control': 'private, max-age=60' });
  }

  let data;
  try {
    data = await readAll(env);
  } catch (error) {
    console.error('Could not read Afterclip usage totals', error);
  }
  return new Response(statsPage(data), {
    status: data ? 200 : 503,
    headers: { ...headers, 'content-type': 'text/html; charset=utf-8', 'cache-control': 'no-store' },
  });
}

export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    if (url.pathname === '/api/ping') return ping(request, env);
    if (url.pathname === '/api/stats' && request.method === 'GET') return stats(request, url, env);
    return json({ error: 'Not found' }, 404);
  },
  async scheduled(_event, env) {
    await env.DB.prepare(`DELETE FROM pings WHERE day < date('now', '-${KEEP_DAYS} days')`).run();
  },
};

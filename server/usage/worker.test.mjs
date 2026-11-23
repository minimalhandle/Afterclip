// Runs the Worker against an in-memory SQLite database that stands in for D1.
// node --test server/usage/worker.test.mjs
import test from 'node:test';
import assert from 'node:assert/strict';
import { DatabaseSync } from 'node:sqlite';
import { readFileSync } from 'node:fs';
import { statsPage } from './stats-page.js';
import worker, { valid, combine } from './worker.js';

const day = (offset = 0) => new Date(Date.now() + offset * 86_400_000).toISOString().slice(0, 10);

const SLEEKMEETER = {
  today: 3, last7Days: 5, last30Days: 8,
  daily: [{ day: day(), users: 3 }, { day: day(-1), users: 2 }],
  versions: [{ version: '0.22.0', users: 8 }],
  editions: [{ edition: 'banana', users: 6 }, { edition: 'potato', users: 2 }],
};

// A service binding that answers like Sleekmeeter's Worker, or fails.
const sleekmeeter = (body = SLEEKMEETER, status = 200) => ({
  fetch: async (request) => {
    assert.equal(new URL(request.url).searchParams.get('format'), 'json');
    return new Response(JSON.stringify(body), { status, headers: { 'content-type': 'application/json' } });
  },
});

function d1(binding = sleekmeeter()) {
  const db = new DatabaseSync(':memory:');
  db.exec(readFileSync(new URL('./schema.sql', import.meta.url), 'utf8'));
  const prepare = (sql) => {
    const make = (args) => ({
      run: async () => db.prepare(sql).run(...args),
      first: async () => db.prepare(sql).get(...args) ?? null,
      all: async () => ({ results: db.prepare(sql).all(...args) }),
    });
    return { bind: (...args) => make(args), ...make([]) };
  };
  return { db, DB: { prepare }, SLEEKMEETER: binding };
}

const ping = (env, body, method = 'POST') =>
  worker.fetch(new Request('https://afterclip.app/api/ping', { method, body: method === 'POST' ? JSON.stringify(body) : undefined }), env);
const id = (n) => n.toString(16).padStart(32, '0');
const stats = (env, query = '', accept = 'application/json') =>
  worker.fetch(new Request('https://afterclip.app/api/stats' + query, { headers: { accept } }), env);

test('counts each install once a day and keeps only id and version', async () => {
  const env = d1();
  assert.equal((await ping(env, { id: id(1), version: '1.6.0' })).status, 204);
  assert.equal((await ping(env, { id: id(1), version: '1.6.0' })).status, 204);
  assert.equal((await ping(env, { id: id(2), version: '1.7.0-beta.1' })).status, 204);
  const rows = env.db.prepare('SELECT * FROM pings').all();
  assert.equal(rows.length, 2);
  assert.deepEqual(Object.keys(rows[0]).sort(), ['day', 'id', 'version']);
  const own = await (await stats(env)).json();
  assert.equal(own.today, 2);
  assert.equal(own.last30Days, 2);
  assert.deepEqual(own.versions.map((v) => v.version).sort(), ['1.6.0', '1.7.0-beta.1']);
});

test('format=all adds Sleekmeeter to the totals, day by day', async () => {
  const env = d1();
  await ping(env, { id: id(1), version: '1.6.0' });
  const all = await (await stats(env, '?format=all')).json();
  assert.equal(all.afterclip.today, 1);
  assert.equal(all.sleekmeeter.today, 3);
  assert.deepEqual([all.total.today, all.total.last7Days, all.total.last30Days], [4, 6, 9]);
  assert.deepEqual(all.total.daily, [
    { day: day(-1), afterclip: 0, sleekmeeter: 2, users: 2 },
    { day: day(), afterclip: 1, sleekmeeter: 3, users: 4 },
  ]);
});

test('when Sleekmeeter cannot be read, the totals are Afterclip alone and say so', async () => {
  for (const binding of [sleekmeeter({ error: 'down' }, 503), sleekmeeter({ nope: true }), { fetch: async () => { throw new Error('offline'); } }]) {
    const env = d1(binding);
    await ping(env, { id: id(1), version: '1.6.0' });
    const all = await (await stats(env, '?format=all')).json();
    assert.equal(all.sleekmeeter, null);
    assert.equal(all.total.today, 1);
    const page = await (await stats(env, '', 'text/html')).text();
    assert.match(page, /Sleekmeeter missing from the totals/);
  }
  assert.equal(combine({ today: 1, last7Days: 1, last30Days: 1, daily: [] }, null).today, 1);
});

test('serves the combined dashboard to browsers and keeps the JSON endpoints', async () => {
  const env = d1();
  await ping(env, { id: id(1), version: '1.6.0' });

  const browser = await stats(env, '', 'text/html,application/xhtml+xml,*/*;q=0.8');
  assert.equal(browser.headers.get('content-type'), 'text/html; charset=utf-8');
  assert.equal(browser.headers.get('cache-control'), 'no-store');
  assert.equal(browser.headers.get('vary'), 'Accept');
  const page = await browser.text();
  assert.match(page, /data-metric="today">4<\/dd>/);
  assert.match(page, /data-app-metric="today">1<\/dd>/);
  const data = JSON.parse(page.match(/id="usage-data">([^<]+)<\/script>/)[1]);
  assert.deepEqual(await (await stats(env, '?format=all', 'text/html')).json(), data);

  for (const accept of ['*/*', 'application/json', 'text/html;q=0,*/*', 'text/html;q=0.2,application/json']) {
    const response = await stats(env, '', accept);
    assert.equal(response.headers.get('content-type'), 'application/json');
    assert.deepEqual(await response.json(), data.afterclip);
  }
  assert.deepEqual(await (await stats(env, '?format=json', 'text/html')).json(), data.afterclip);
});

test('embedded dashboard data cannot close the JSON script element', () => {
  const hostile = '</script><script>alert(1)</script>';
  const afterclip = { today: 1, last7Days: 1, last30Days: 1, daily: [], versions: [{ version: hostile, users: 1 }] };
  const page = statsPage({ total: combine(afterclip, null), afterclip, sleekmeeter: null });
  assert.equal(page.includes(hostile), false);
  const data = JSON.parse(page.match(/id="usage-data">([^<]+)<\/script>/)[1]);
  assert.equal(data.afterclip.versions[0].version, hostile);
});

test('rejects anything that is not the expected ping', async () => {
  const env = d1();
  for (const bad of [{}, { id: 'x', version: '1.6.0' }, { id: id(1), version: '1' }, { id: id(1), version: '1.6.0-<b>' }, { id: id(1) }]) {
    assert.equal((await ping(env, bad)).status, 400);
  }
  assert.equal((await ping(env, null, 'GET')).status, 405);
  assert.equal((await worker.fetch(new Request('https://afterclip.app/api/other'), env)).status, 404);
  assert.equal(env.db.prepare('SELECT COUNT(*) AS n FROM pings').get().n, 0);
  assert.equal(valid({ id: id(3), version: '1.6.0', extra: 1 }), true, 'extra fields are ignored, not stored');
});

test('the daily clean-up removes rows older than 400 days', async () => {
  const env = d1();
  env.db.prepare("INSERT INTO pings VALUES (date('now', '-401 days'), ?, '1.0.0'), (date('now'), ?, '1.6.0')").run(id(1), id(2));
  await worker.scheduled({}, env);
  assert.equal(env.db.prepare('SELECT COUNT(*) AS n FROM pings').get().n, 1);
});

// The usage dashboard at afterclip.app/api/stats: users of Afterclip and Sleekmeeter together, then each app.

const formatNumber = new Intl.NumberFormat('en');

const styles = String.raw`
:root{
  color-scheme:dark;
  --bg:#0e0c1f;--surface:#171433;--surface-2:#221e47;--line:#302b5c;
  --text:#f0eefc;--muted:#9a95bd;--accent:#7c6cff;--accent-text:#a99dff;--ok:#45d39a;--rec:#f0554f;
  --afterclip:#7c6cff;--sleekmeeter:#45d39a;
}
*{box-sizing:border-box}
[hidden]{display:none!important}
body{margin:0;background:var(--bg);color:var(--text);font:16px/1.55 Inter,"Segoe UI",system-ui,sans-serif;-webkit-font-smoothing:antialiased}
h1,h2,p,figure,dl,dd{margin:0}
a{color:var(--accent-text)}
button{font:inherit;color:inherit;cursor:pointer}
:focus-visible{outline:2px solid var(--accent);outline-offset:3px}
.shell{width:min(100% - 48px,1080px);margin-inline:auto}
.header{border-bottom:1px solid var(--line)}
.header .shell{min-height:64px;display:flex;align-items:center;justify-content:space-between;gap:12px}
.brand{display:inline-flex;align-items:center;gap:10px;font-size:18px;font-weight:700;color:var(--text);text-decoration:none}
.brand img{width:28px;height:28px}
.control{min-height:36px;display:inline-flex;align-items:center;gap:6px;padding:6px 12px;border:1px solid var(--line);border-radius:10px;background:var(--surface);color:var(--text);font-size:14px;font-weight:600;text-decoration:none}
.control:hover{background:var(--surface-2)}
.control:disabled{cursor:wait;opacity:.65}
main{padding:clamp(36px,6vw,72px) 0}
.kicker,.metric dt,.live,.dates,.chart-scale,.legend{font-size:12px;font-weight:600;letter-spacing:.06em;text-transform:uppercase}
.kicker{color:var(--accent-text);margin-bottom:10px}
.intro{display:flex;align-items:end;justify-content:space-between;gap:20px;margin-bottom:26px}
.intro h1{font-size:clamp(40px,6vw,64px);font-weight:800;letter-spacing:-.035em;line-height:1.04}
.intro p{max-width:42ch;color:var(--muted)}
.metrics{display:grid;grid-template-columns:1.6fr repeat(2,1fr);gap:12px}
.panel{border:1px solid var(--line);border-radius:20px;background:var(--surface)}
.metric{padding:clamp(16px,3vw,28px);display:grid;align-content:center;min-height:132px}
.metric-today{background:linear-gradient(160deg,rgba(124,108,255,.28),var(--surface) 70%)}
.metric dt{color:var(--muted)}
.metric-today dt{color:var(--accent-text)}
.metric dd{font-size:clamp(36px,5.5vw,64px);font-weight:800;line-height:1.05;letter-spacing:-.04em;font-variant-numeric:tabular-nums}
.metric-today dd{font-size:clamp(48px,8vw,96px)}
.apps{display:grid;grid-template-columns:1fr 1fr;gap:12px;margin-top:12px}
.app{padding:18px 22px}
.app h2{display:flex;align-items:center;gap:8px;font-size:16px;font-weight:700}
.swatch{width:10px;height:10px;border-radius:3px;flex:none}
.app dl{display:grid;grid-template-columns:repeat(3,1fr);gap:8px;margin-top:10px}
.app dt{color:var(--muted);font-size:12px}
.app dd{font-size:26px;font-weight:700;font-variant-numeric:tabular-nums}
.app .offline{color:var(--muted);font-size:14px;margin-top:10px}
.update-row{display:flex;align-items:center;justify-content:space-between;gap:12px;margin:14px 0 28px}
.live{display:flex;align-items:center;gap:9px;color:var(--muted)}
.live-dot{height:7px;width:7px;border-radius:50%;background:var(--ok);box-shadow:0 0 10px var(--ok);flex:none}
.live[data-state="error"] .live-dot{background:var(--rec);box-shadow:0 0 10px var(--rec)}
.chart,.breakdown{padding:clamp(18px,3vw,28px)}
.panel-heading{display:flex;align-items:start;justify-content:space-between;gap:12px}
.panel-heading h2,.breakdown h2{font-size:20px;font-weight:700;letter-spacing:-.02em;line-height:1.25}
.panel-heading p{color:var(--muted);font-size:14px;margin-top:3px}
.chart-scale{color:var(--muted);text-align:right;white-space:nowrap}
.legend{display:flex;gap:16px;color:var(--muted);margin-top:12px}
.legend span{display:inline-flex;align-items:center;gap:6px}
.bars{display:grid;grid-auto-flow:column;grid-auto-columns:minmax(0,1fr);align-items:end;gap:clamp(2px,.6vw,7px);height:140px;margin-top:14px;border-bottom:1px solid var(--line);background:repeating-linear-gradient(to top,transparent 0 calc(25% - 1px),var(--line) calc(25% - 1px) 25%)}
.bar{height:var(--bar-height);min-height:2px;display:flex;flex-direction:column-reverse;border-radius:4px 4px 0 0;overflow:hidden;opacity:.75}
.bar:hover,.bar:last-child{opacity:1}
.bar i{display:block;flex:none}
.bar .a{background:var(--afterclip)}
.bar .s{background:var(--sleekmeeter)}
.dates{display:flex;justify-content:space-between;gap:8px;color:var(--muted);margin-top:7px}
.daily-details{margin-top:13px}
.daily-details summary{cursor:pointer;font-size:14px;font-weight:600}
table{border-collapse:collapse;width:100%;font-size:14px;margin-top:6px;font-variant-numeric:tabular-nums}
th,td{padding:6px 0;border-bottom:1px solid var(--line);text-align:right;font-weight:400}
th:first-child{text-align:left}
thead th{font-weight:600}
.breakdowns{display:grid;grid-template-columns:repeat(3,1fr);gap:12px;margin-top:12px}
.breakdown ul{list-style:none;margin:9px 0 0;padding:0}
.breakdown li{display:flex;justify-content:space-between;gap:12px;border-bottom:1px solid var(--line);padding:7px 0;font-size:15px}
.breakdown li:last-child{border-bottom:0}
.breakdown strong{font-variant-numeric:tabular-nums}
.note{margin-top:15px;font-size:14px;color:var(--muted)}
.footer{border-top:1px solid var(--line);color:var(--muted);font-size:14px}
.footer .shell{display:flex;align-items:center;justify-content:space-between;gap:15px;padding:18px 0}
.footer nav{display:flex;gap:16px;flex-wrap:wrap}
@media(max-width:760px){
  .shell{width:calc(100% - 32px)}
  .intro{display:block}
  .intro p{margin-top:11px}
  .metrics{grid-template-columns:repeat(2,minmax(0,1fr));gap:8px}
  .metric{min-height:90px}
  .metric-today{grid-column:1/-1;min-height:112px}
  .apps,.breakdowns{grid-template-columns:1fr}
  .bars{height:100px}
  .footer .shell{display:block}
  .footer nav{margin-top:8px}
}
`;

const clientScript = String.raw`
const number = new Intl.NumberFormat('en');
const shortDate = new Intl.DateTimeFormat('en', {month: 'short', day: 'numeric', timeZone: 'UTC'});
const longDate = new Intl.DateTimeFormat('en', {dateStyle: 'medium', timeZone: 'UTC'});
const editionNames = new Map([['standard', 'Standard'], ['banana', 'Banana'], ['potato', 'Potato'], ['unknown', 'Unknown']]);
const status = document.querySelector('#live-status');
const refreshButton = document.querySelector('#refresh');
let inFlight = false;

function dailyRows(daily) {
  const values = new Map(daily.map((row) => [row.day, row]));
  const now = new Date();
  return Array.from({length: 30}, (_, index) => {
    const date = new Date(Date.UTC(now.getUTCFullYear(), now.getUTCMonth(), now.getUTCDate() - 29 + index));
    const row = values.get(date.toISOString().slice(0, 10)) || {};
    return {date, afterclip: row.afterclip || 0, sleekmeeter: row.sleekmeeter || 0};
  });
}

function cell(tag, text) {
  const element = document.createElement(tag);
  element.textContent = text;
  return element;
}

function drawChart(daily) {
  const rows = dailyRows(daily);
  const peak = Math.max(...rows.map((row) => row.afterclip + row.sleekmeeter));
  document.querySelector('#chart-scale').textContent = 'Peak ' + number.format(peak);
  document.querySelector('#chart-start').textContent = shortDate.format(rows[0].date);
  document.querySelector('#chart-end').textContent = shortDate.format(rows[rows.length - 1].date);
  const bars = [];
  const tableRows = [];
  for (const {date, afterclip, sleekmeeter} of rows) {
    const total = afterclip + sleekmeeter;
    const bar = document.createElement('span');
    bar.className = 'bar';
    bar.style.setProperty('--bar-height', (100 * total / Math.max(peak, 1)) + '%');
    bar.title = longDate.format(date) + ' · ' + number.format(total) + ' (Afterclip ' + number.format(afterclip) + ', Sleekmeeter ' + number.format(sleekmeeter) + ')';
    for (const [name, count] of [['a', afterclip], ['s', sleekmeeter]]) {
      const part = document.createElement('i');
      part.className = name;
      part.style.height = (100 * count / Math.max(total, 1)) + '%';
      bar.append(part);
    }
    bars.push(bar);

    const row = document.createElement('tr');
    const day = cell('th', longDate.format(date));
    day.scope = 'row';
    row.append(day, cell('td', number.format(afterclip)), cell('td', number.format(sleekmeeter)), cell('td', number.format(total)));
    tableRows.push(row);
  }
  document.querySelector('#daily-bars').replaceChildren(...bars);
  document.querySelector('#daily-rows').replaceChildren(...tableRows);
}

function drawBreakdown(id, rows, label, missing) {
  const list = document.querySelector(id);
  list.replaceChildren();
  for (const row of rows || []) {
    const item = document.createElement('li');
    item.append(cell('span', label(row)), cell('strong', number.format(row.users)));
    list.append(item);
  }
  if (!rows || !rows.length) list.append(cell('li', rows ? 'No reports in this period.' : missing));
}

function paintApp(name, data) {
  const panel = document.querySelector('[data-app="' + name + '"]');
  panel.querySelector('dl').hidden = !data;
  panel.querySelector('.offline').hidden = !!data;
  for (const metric of panel.querySelectorAll('[data-app-metric]')) {
    metric.textContent = data ? number.format(data[metric.dataset.appMetric]) : '—';
  }
}

function paint(data) {
  for (const metric of document.querySelectorAll('[data-metric]')) {
    metric.textContent = number.format(data.total[metric.dataset.metric]);
  }
  paintApp('afterclip', data.afterclip);
  paintApp('sleekmeeter', data.sleekmeeter);
  drawChart(data.total.daily);
  const away = 'Sleekmeeter\'s numbers can\'t be read right now.';
  drawBreakdown('#afterclip-versions', data.afterclip.versions, ({version}) => version);
  drawBreakdown('#sleekmeeter-versions', data.sleekmeeter && data.sleekmeeter.versions, ({version}) => version, away);
  drawBreakdown('#editions', data.sleekmeeter && data.sleekmeeter.editions, ({edition}) => editionNames.get(edition) || edition, away);
  document.querySelector('#data-details').hidden = false;
}

function checked(data) {
  const time = new Date().toLocaleTimeString([], {hour: 'numeric', minute: '2-digit'});
  status.dataset.state = data.sleekmeeter ? 'live' : 'error';
  status.querySelector('span:last-child').textContent = 'Last checked ' + time + ' · Refreshes every minute'
    + (data.sleekmeeter ? '' : ' · Sleekmeeter missing from the totals');
}

async function refresh(cache = 'default') {
  if (inFlight) return;
  inFlight = true;
  refreshButton.disabled = true;
  try {
    const url = new URL(location.href);
    url.searchParams.set('format', 'all');
    const response = await fetch(url, {headers: {Accept: 'application/json'}, cache, signal: AbortSignal.timeout(10000)});
    if (!response.ok) throw new Error('Usage request failed');
    const data = await response.json();
    paint(data);
    checked(data);
  } catch {
    status.dataset.state = 'error';
    status.querySelector('span:last-child').textContent = 'Could not refresh. Trying again in a minute.';
  } finally {
    refreshButton.disabled = false;
    inFlight = false;
  }
}

const initial = JSON.parse(document.querySelector('#usage-data').textContent);
if (initial) {
  paint(initial);
  checked(initial);
}
refreshButton.hidden = false;
refreshButton.addEventListener('click', () => refresh('reload'));
setInterval(() => {
  if (!document.hidden) refresh();
}, 60000);
document.addEventListener('visibilitychange', () => {
  if (!document.hidden) refresh();
});
`;

function appPanel(name, title, data) {
  const value = (key) => data ? formatNumber.format(data[key]) : '—';
  return `<section class="app panel" data-app="${name}" aria-label="${title}">
        <h2><span class="swatch" style="background:var(--${name})"></span>${title}</h2>
        <dl${data ? '' : ' hidden'}>
          <div><dt>Today</dt><dd data-app-metric="today">${value('today')}</dd></div>
          <div><dt>7 days</dt><dd data-app-metric="last7Days">${value('last7Days')}</dd></div>
          <div><dt>30 days</dt><dd data-app-metric="last30Days">${value('last30Days')}</dd></div>
        </dl>
        <p class="offline"${data ? ' hidden' : ''}>Can't be read right now, so it's left out of the totals.</p>
      </section>`;
}

export function statsPage(data) {
  const total = (key) => data ? formatNumber.format(data.total[key]) : '—';
  const embedded = JSON.stringify(data ?? null).replaceAll('<', '\\u003c');
  const live = data && data.sleekmeeter;
  const statusText = !data ? 'Usage counts are temporarily unavailable'
    : data.sleekmeeter ? 'Counts are up to a minute old' : 'Sleekmeeter missing from the totals';
  return `<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Afterclip and Sleekmeeter usage stats</title>
  <meta name="description" content="Live anonymous usage totals for Afterclip and Sleekmeeter.">
  <meta name="robots" content="noindex">
  <meta name="theme-color" content="#0e0c1f">
  <link rel="icon" type="image/svg+xml" href="/assets/afterclip-icon.svg">
  <link rel="preconnect" href="https://fonts.googleapis.com">
  <link rel="preconnect" href="https://fonts.gstatic.com" crossorigin>
  <link href="https://fonts.googleapis.com/css2?family=Inter:wght@400;600;700;800&display=swap" rel="stylesheet">
  <style>${styles}</style>
</head>
<body>
  <header class="header">
    <div class="shell">
      <a class="brand" href="/"><img src="/assets/afterclip-mark.svg" alt="">Afterclip</a>
      <a class="control" href="/">Home <span aria-hidden="true">↗</span></a>
    </div>
  </header>
  <main class="shell">
    <p class="kicker">Afterclip + Sleekmeeter / Usage</p>
    <div class="intro">
      <h1>Usage stats.</h1>
      <p>Installations of both apps that choose to share anonymous usage data. Each one reports at most once per UTC day.</p>
    </div>
    <dl class="metrics" aria-label="Opted-in installations of both apps">
      <div class="metric metric-today panel"><dt>Both apps today (UTC)</dt><dd data-metric="today">${total('today')}</dd></div>
      <div class="metric panel"><dt>Last 7 days</dt><dd data-metric="last7Days">${total('last7Days')}</dd></div>
      <div class="metric panel"><dt>Last 30 days</dt><dd data-metric="last30Days">${total('last30Days')}</dd></div>
    </dl>
    <div class="apps">
      ${appPanel('afterclip', 'Afterclip', data && data.afterclip)}
      ${appPanel('sleekmeeter', 'Sleekmeeter', data && data.sleekmeeter)}
    </div>
    <div class="update-row">
      <p class="live" id="live-status" role="status" data-state="${live ? 'live' : 'error'}"><span class="live-dot" aria-hidden="true"></span><span>${statusText}</span></p>
      <button class="control" id="refresh" type="button" hidden>Refresh <span aria-hidden="true">↻</span></button>
    </div>
    <div id="data-details" hidden>
      <section class="chart panel" aria-labelledby="daily-title">
        <div class="panel-heading">
          <div><h2 id="daily-title">Daily reports</h2><p>Opted-in installations across the last 30 UTC days</p></div>
          <span class="chart-scale" id="chart-scale"></span>
        </div>
        <div class="legend" aria-hidden="true"><span><i class="swatch" style="background:var(--afterclip)"></i>Afterclip</span><span><i class="swatch" style="background:var(--sleekmeeter)"></i>Sleekmeeter</span></div>
        <div class="bars" id="daily-bars" aria-hidden="true"></div>
        <div class="dates" aria-hidden="true"><span id="chart-start"></span><span id="chart-end"></span></div>
        <details class="daily-details">
          <summary>View daily counts</summary>
          <table>
            <thead><tr><th scope="col">Date (UTC)</th><th scope="col">Afterclip</th><th scope="col">Sleekmeeter</th><th scope="col">Both</th></tr></thead>
            <tbody id="daily-rows"></tbody>
          </table>
        </details>
      </section>
      <div class="breakdowns">
        <section class="breakdown panel" aria-labelledby="afterclip-version-title"><h2 id="afterclip-version-title">Afterclip versions</h2><ul id="afterclip-versions"></ul></section>
        <section class="breakdown panel" aria-labelledby="sleekmeeter-version-title"><h2 id="sleekmeeter-version-title">Sleekmeeter versions</h2><ul id="sleekmeeter-versions"></ul></section>
        <section class="breakdown panel" aria-labelledby="edition-title"><h2 id="edition-title">Voicemeeter editions</h2><ul id="editions"></ul></section>
      </div>
      <p class="note">Each app has its own random ID, so someone who uses both is counted once in each. The version and edition lists cover the last 30 days; an installation that updated may appear in more than one row.</p>
    </div>
  </main>
  <footer class="footer"><div class="shell"><p>Afterclip · Free and open source</p><nav><a href="?format=all">JSON for both apps</a><a href="?format=json">Afterclip JSON</a><a href="https://sleekmeeter.app/api/stats">Sleekmeeter dashboard</a></nav></div></footer>
  <script type="application/json" id="usage-data">${embedded}</script>
  <script>${clientScript}</script>
</body>
</html>`;
}

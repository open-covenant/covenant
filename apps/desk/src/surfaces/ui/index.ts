/**
 * The desk page, served by the daemon at `GET /`.
 *
 * One HTML document, one script, one stylesheet. No framework and no build
 * step. All three are emitted from this module as strings so `tsc` alone
 * produces a working package. The page reads the same HTTP API as everything
 * else, with the bearer token from the config file.
 *
 * Sections: status header (session state, block, uptime, dry run or live),
 * premium table refreshing every 15 seconds, quote panel, orders table with
 * cancel, hedge panel, and a 24 hour premium chart for a chosen symbol.
 */

export interface UiSurface {
  /** The page. */
  html(): string;
  /** The script the page loads from `GET /app.js`. */
  script(): string;
  /** The stylesheet the page loads from `GET /app.css`. */
  styles(): string;
}

/** How often the premium table refreshes, milliseconds. */
export const UI_REFRESH_MS = 15_000;

export function createUiSurface(): UiSurface {
  return { html: () => HTML, script: () => SCRIPT, styles: () => STYLES };
}

const HTML = `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="color-scheme" content="dark">
<title>Covenant Desk</title>
<link rel="stylesheet" href="/app.css">
</head>
<body>
<header class="bar">
  <div class="brand">
    <span class="mark"></span>
    <span>Covenant Desk</span>
  </div>
  <dl class="facts" id="facts">
    <div><dt>Session</dt><dd id="fact-session">reading</dd></div>
    <div><dt>Block</dt><dd id="fact-block">reading</dd></div>
    <div><dt>Mode</dt><dd id="fact-mode">reading</dd></div>
    <div><dt>Running for</dt><dd id="fact-uptime">reading</dd></div>
  </dl>
</header>

<main>
  <section id="connect" class="panel hidden">
    <h2>Connect to your desk</h2>
    <p class="note">Paste the token from your config file. Run <code>covenant-desk status</code> to print the page link with the token in it.</p>
    <form id="connect-form" class="row">
      <input id="token-input" type="password" placeholder="Bearer token" autocomplete="off" spellcheck="false">
      <button type="submit">Connect</button>
    </form>
  </section>

  <section id="desk" class="hidden">
    <section class="panel">
      <div class="panel-head">
        <h2>Premium</h2>
        <span class="note" id="premium-note">On-chain price against the reference price, refreshed every 15 seconds.</span>
      </div>
      <table id="premium-table">
        <thead>
          <tr><th>Symbol</th><th class="num">On chain</th><th class="num">Reference</th><th>Source</th><th class="num">Premium</th><th class="num">Liquidity</th><th></th></tr>
        </thead>
        <tbody id="premium-rows"><tr><td colspan="7" class="note">Reading prices.</td></tr></tbody>
      </table>
    </section>

    <div class="split">
      <section class="panel">
        <h2>Quote</h2>
        <form id="quote-form" class="row">
          <input id="quote-token" placeholder="Symbol or address" value="NVDA" spellcheck="false">
          <input id="quote-usd" type="number" min="1" step="1" value="100" aria-label="Size in dollars">
          <select id="quote-side" aria-label="Side">
            <option value="buy">Buy</option>
            <option value="sell">Sell</option>
          </select>
          <button type="submit">Price it</button>
        </form>
        <div id="quote-result" class="result note">Prices a buy or a sell of that dollar size, on chain and at fair value.</div>
      </section>

      <section class="panel">
        <h2>Hedge</h2>
        <form id="hedge-form" class="row">
          <input id="hedge-symbol" placeholder="Stock symbol" value="NVDA" spellcheck="false">
          <input id="hedge-usd" type="number" min="1" step="1" value="1000" aria-label="Stock exposure in dollars">
          <button type="submit">Size the short</button>
          <button type="button" id="hedge-unwind" class="ghost">Unwind</button>
        </form>
        <div id="hedge-result" class="result note">Sizes the short that cancels the stock exposure inside a position.</div>
        <table id="hedge-table" class="hidden">
          <thead><tr><th>Symbol</th><th class="num">Size</th><th class="num">Mark</th><th class="num">Funding 8h</th></tr></thead>
          <tbody id="hedge-rows"></tbody>
        </table>
      </section>
    </div>

    <section class="panel">
      <div class="panel-head">
        <h2>Orders</h2>
        <span class="note">Orders are dry runs until live execution is turned on in the config file and asked for on the order.</span>
      </div>
      <table id="orders-table">
        <thead>
          <tr><th>Id</th><th>Kind</th><th>Side</th><th class="num">Size in</th><th>Trigger</th><th>Status</th><th></th></tr>
        </thead>
        <tbody id="orders-rows"><tr><td colspan="7" class="note">No orders yet.</td></tr></tbody>
      </table>
    </section>

    <section class="panel">
      <div class="panel-head">
        <h2>Recorder</h2>
        <span class="note" id="recorder-note">Premium over the last 24 hours.</span>
      </div>
      <div class="row">
        <input id="recorder-symbol" placeholder="Symbol" value="NVDA" spellcheck="false">
        <button type="button" id="recorder-load">Show</button>
        <button type="button" id="recorder-toggle" class="ghost">Start recording</button>
      </div>
      <div id="sparkline" class="sparkline"></div>
    </section>
  </section>

  <p id="error" class="error hidden" role="alert"></p>
</main>

<script src="/app.js" type="module"></script>
</body>
</html>
`;

const STYLES = `:root {
  color-scheme: dark;
  --bg: #0b0d10;
  --panel: #12151a;
  --line: #222831;
  --text: #e8eaed;
  --muted: #8b95a5;
  --up: #4ade80;
  --down: #f87171;
  --accent: #7dd3fc;
}

* { box-sizing: border-box; }

body {
  margin: 0;
  background: var(--bg);
  color: var(--text);
  font: 14px/1.5 ui-sans-serif, -apple-system, "Segoe UI", Roboto, Helvetica, Arial, sans-serif;
}

.bar {
  display: flex;
  flex-wrap: wrap;
  gap: 24px;
  align-items: center;
  justify-content: space-between;
  padding: 16px 24px;
  border-bottom: 1px solid var(--line);
  background: var(--panel);
  position: sticky;
  top: 0;
  z-index: 2;
}

.brand { display: flex; align-items: center; gap: 10px; font-weight: 600; letter-spacing: 0.01em; }
.mark { width: 10px; height: 10px; border-radius: 50%; background: var(--accent); }

.facts { display: flex; gap: 28px; margin: 0; }
.facts div { display: flex; flex-direction: column; gap: 2px; }
.facts dt { color: var(--muted); font-size: 11px; text-transform: uppercase; letter-spacing: 0.06em; }
.facts dd { margin: 0; font-variant-numeric: tabular-nums; }

main { max-width: 1180px; margin: 0 auto; padding: 24px; display: flex; flex-direction: column; gap: 20px; }

.panel { background: var(--panel); border: 1px solid var(--line); border-radius: 10px; padding: 18px 20px; }
.panel-head { display: flex; flex-wrap: wrap; align-items: baseline; gap: 12px; justify-content: space-between; }
.panel h2 { margin: 0 0 12px; font-size: 15px; font-weight: 600; }
.panel-head h2 { margin-bottom: 12px; }
.split { display: grid; grid-template-columns: 1fr 1fr; gap: 20px; }
@media (max-width: 880px) { .split { grid-template-columns: 1fr; } .facts { gap: 18px; } }

.row { display: flex; flex-wrap: wrap; gap: 8px; align-items: center; margin-bottom: 12px; }

input, select {
  background: #0d1015;
  color: var(--text);
  border: 1px solid var(--line);
  border-radius: 6px;
  padding: 8px 10px;
  font: inherit;
  min-width: 0;
}
input:focus, select:focus { outline: 2px solid var(--accent); outline-offset: -1px; }
select { cursor: pointer; }

button {
  cursor: pointer;
  background: var(--accent);
  color: #06202b;
  border: 1px solid transparent;
  border-radius: 6px;
  padding: 8px 14px;
  font: inherit;
  font-weight: 600;
}
button:hover { filter: brightness(1.08); }
button.ghost { background: transparent; color: var(--text); border-color: var(--line); font-weight: 500; }
button.link { background: none; color: var(--accent); border: none; padding: 4px 6px; font-weight: 500; }

table { width: 100%; border-collapse: collapse; }
th, td { text-align: left; padding: 8px 10px; border-bottom: 1px solid var(--line); }
th { color: var(--muted); font-size: 11px; text-transform: uppercase; letter-spacing: 0.06em; font-weight: 600; }
tbody tr:last-child td { border-bottom: none; }
.num { text-align: right; font-variant-numeric: tabular-nums; }
.pos { color: var(--up); }
.neg { color: var(--down); }

.note { color: var(--muted); font-size: 12px; }
.result { margin-top: 4px; }
.result dl { display: grid; grid-template-columns: max-content 1fr; gap: 4px 16px; margin: 0; }
.result dt { color: var(--muted); }
.result dd { margin: 0; font-variant-numeric: tabular-nums; }

.sparkline { margin-top: 8px; }
.sparkline svg { width: 100%; height: 120px; }
.sparkline path { fill: none; stroke: var(--accent); stroke-width: 1.5; }
.sparkline line { stroke: var(--line); stroke-width: 1; }

code { background: #0d1015; border: 1px solid var(--line); border-radius: 4px; padding: 1px 5px; }
.error { color: var(--down); }
.hidden { display: none; }
`;

const SCRIPT = `const REFRESH_MS = ${UI_REFRESH_MS};
const DAY_MS = 86400000;
const store = window.sessionStorage;
let token = '';
let timer = null;

function byId(id) { return document.getElementById(id); }

function show(node, visible) { node.classList.toggle('hidden', !visible); }

function text(node, value) { node.textContent = value; }

function setError(message) {
  const box = byId('error');
  text(box, message || '');
  show(box, Boolean(message));
}

async function api(path, options) {
  const request = Object.assign({ headers: {} }, options || {});
  request.headers = Object.assign({ Authorization: 'Bearer ' + token }, request.headers);
  const response = await fetch(path, request);
  const body = await response.json().catch(function () { return {}; });
  if (!response.ok) {
    if (response.status === 401) { forget(); }
    throw new Error(body.reason || ('The desk answered ' + response.status + '.'));
  }
  return body;
}

function forget() {
  store.removeItem('covenant-desk-token');
  token = '';
  show(byId('connect'), true);
  show(byId('desk'), false);
  if (timer) { clearInterval(timer); timer = null; }
}

function num(value, digits) {
  if (value === null || value === undefined || Number.isNaN(value)) return '';
  return Number(value).toLocaleString(undefined, { minimumFractionDigits: digits, maximumFractionDigits: digits });
}

function usd(quantity) {
  if (!quantity || quantity.value === undefined) return '';
  const value = Number(quantity.value);
  const size = Math.abs(value);
  // A token quoted in a stock token often trades far below a cent, so the
  // number of decimals follows the size instead of being fixed at four.
  if (size >= 10) return '$' + num(value, 2);
  if (size >= 0.01 || size === 0) return '$' + num(value, 4);
  return '$' + value.toFixed(Math.min(18, Math.ceil(-Math.log10(size)) + 3));
}

function bps(quantity) {
  if (!quantity || quantity.value === undefined) return '';
  const value = Number(quantity.value);
  const sign = value > 0 ? '+' : '';
  return sign + num(value, 0) + ' bps';
}

function compact(value) {
  if (value === null || value === undefined) return '';
  const n = Number(value);
  if (!Number.isFinite(n)) return String(value);
  if (n >= 1e12) return num(n / 1e12, 1) + 'T';
  if (n >= 1e9) return num(n / 1e9, 1) + 'B';
  if (n >= 1e6) return num(n / 1e6, 1) + 'M';
  if (n >= 1e3) return num(n / 1e3, 1) + 'k';
  return num(n, 0);
}

function duration(seconds) {
  if (!Number.isFinite(seconds)) return '';
  if (seconds < 60) return Math.round(seconds) + ' s';
  if (seconds < 3600) return Math.round(seconds / 60) + ' min';
  if (seconds < 86400) return (seconds / 3600).toFixed(1) + ' h';
  return (seconds / 86400).toFixed(1) + ' days';
}

function cell(row, value, className) {
  const td = document.createElement('td');
  if (className) td.className = className;
  if (value instanceof Node) td.appendChild(value); else text(td, value === undefined || value === null ? '' : String(value));
  row.appendChild(td);
  return td;
}

function fill(tbody, rows, columns, empty) {
  tbody.replaceChildren();
  if (!rows.length) {
    const row = document.createElement('tr');
    const td = cell(row, empty, 'note');
    td.colSpan = columns;
    tbody.appendChild(row);
    return;
  }
  rows.forEach(function (build) { tbody.appendChild(build()); });
}

async function loadStatus() {
  const status = await api('/v1/status');
  text(byId('fact-session'), status.sessionState);
  text(byId('fact-block'), status.blockNumber ? Number(status.blockNumber).toLocaleString() : 'not read yet');
  text(byId('fact-mode'), status.live ? 'live execution' : 'dry run');
  text(byId('fact-uptime'), duration(status.uptimeSec));
  const toggle = byId('recorder-toggle');
  toggle.dataset.running = status.recorderRunning ? '1' : '0';
  text(toggle, status.recorderRunning ? 'Stop recording' : 'Start recording');
  if (status.degraded && status.degraded.length) {
    setError(status.degraded.map(function (item) { return item.component + ': ' + item.reason; }).join(' '));
  }
}

async function loadPremium() {
  const body = await api('/v1/premium?limit=25');
  const rows = (body.premium || []).map(function (item) {
    return function () {
      const row = document.createElement('tr');
      cell(row, item.symbol);
      cell(row, usd(item.onchainMid), 'num');
      cell(row, usd(item.reference), 'num');
      cell(row, item.referenceSource || '');
      const premium = cell(row, bps(item.premiumBps), 'num');
      if (item.premiumBps) premium.classList.add(item.premiumBps.value >= 0 ? 'pos' : 'neg');
      cell(row, compact(item.liquidity), 'num');
      const button = document.createElement('button');
      button.type = 'button';
      button.className = 'link';
      text(button, 'chart');
      button.addEventListener('click', function () {
        byId('recorder-symbol').value = item.symbol;
        loadSparkline().catch(function (error) { setError(error.message); });
      });
      cell(row, button, 'num');
      return row;
    };
  });
  fill(byId('premium-rows'), rows, 7, 'No stock token has a priced pool yet.');
  text(byId('premium-note'), 'Refreshed ' + new Date(body.asOf || Date.now()).toLocaleTimeString() + '. Updates every 15 seconds.');
}

async function loadOrders() {
  const body = await api('/v1/orders?limit=50');
  const rows = (body.orders || []).map(function (order) {
    return function () {
      const row = document.createElement('tr');
      cell(row, order.id.slice(0, 8));
      cell(row, order.kind);
      cell(row, order.side);
      cell(row, order.amountIn, 'num');
      cell(row, describeTrigger(order.trigger));
      cell(row, order.status + (order.live ? ' (live)' : ' (dry run)'));
      if (order.status === 'open') {
        const button = document.createElement('button');
        button.type = 'button';
        button.className = 'link';
        text(button, 'cancel');
        button.addEventListener('click', function () {
          api('/v1/orders/' + order.id, { method: 'DELETE' })
            .then(loadOrders)
            .catch(function (error) { setError(error.message); });
        });
        cell(row, button, 'num');
      } else {
        cell(row, '', 'num');
      }
      return row;
    };
  });
  fill(byId('orders-rows'), rows, 7, 'No orders yet.');
}

function describeTrigger(trigger) {
  if (!trigger) return '';
  const parts = [];
  const basis = trigger.priceBasis === 'usdFair' ? 'fair' : 'on chain';
  if (trigger.priceLte !== undefined) parts.push(basis + ' at or below $' + num(trigger.priceLte, 2));
  if (trigger.priceGte !== undefined) parts.push(basis + ' at or above $' + num(trigger.priceGte, 2));
  if (trigger.premiumLteBps !== undefined) parts.push('premium at or below ' + trigger.premiumLteBps + ' bps');
  if (trigger.premiumGteBps !== undefined) parts.push('premium at or above ' + trigger.premiumGteBps + ' bps');
  if (trigger.atNextOpenOffsetSec !== undefined) parts.push('at the next open');
  if (trigger.at !== undefined) parts.push(new Date(trigger.at).toLocaleString());
  return parts.join(', ') || 'no condition';
}

async function loadHedge() {
  const body = await api('/v1/hedge');
  const positions = body.positions || [];
  show(byId('hedge-table'), positions.length > 0);
  const rows = positions.map(function (position) {
    return function () {
      const row = document.createElement('tr');
      cell(row, position.symbol);
      cell(row, num(position.sizeBase && position.sizeBase.value, 4), 'num');
      cell(row, usd(position.markPrice), 'num');
      cell(row, bps(position.fundingBps8h), 'num');
      return row;
    };
  });
  fill(byId('hedge-rows'), rows, 4, body.reason || 'No hedge is open.');
  if (body.reason && positions.length > 0) setError(body.reason);
}

async function loadSparkline() {
  const symbol = byId('recorder-symbol').value.trim().toUpperCase();
  if (!symbol) return;
  const since = Date.now() - DAY_MS;
  const body = await api('/v1/recorder?symbol=' + encodeURIComponent(symbol) + '&since=' + since);
  const points = (body.observations || []).filter(function (row) { return row.premiumBps !== null && row.premiumBps !== undefined; });
  drawSparkline(points);
  text(
    byId('recorder-note'),
    points.length
      ? symbol + ': ' + points.length + ' observations over the last 24 hours.'
      : 'No observations for ' + symbol + ' in the last 24 hours. Start recording to collect them.',
  );
}

function drawSparkline(points) {
  const host = byId('sparkline');
  host.replaceChildren();
  if (points.length < 2) return;
  const width = 1000;
  const height = 120;
  const values = points.map(function (point) { return Number(point.premiumBps); });
  const times = points.map(function (point) { return Number(point.ts); });
  const lowest = Math.min.apply(null, values.concat([0]));
  const highest = Math.max.apply(null, values.concat([0]));
  const span = highest - lowest || 1;
  const first = times[0];
  const last = times[times.length - 1];
  const acrossSpan = last - first || 1;
  const x = function (t) { return ((t - first) / acrossSpan) * width; };
  const y = function (v) { return height - ((v - lowest) / span) * height; };

  const ns = 'http://www.w3.org/2000/svg';
  const svg = document.createElementNS(ns, 'svg');
  svg.setAttribute('viewBox', '0 0 ' + width + ' ' + height);
  svg.setAttribute('preserveAspectRatio', 'none');
  svg.setAttribute('role', 'img');
  svg.setAttribute('aria-label', 'Premium in basis points over the last 24 hours');

  const zero = document.createElementNS(ns, 'line');
  zero.setAttribute('x1', '0');
  zero.setAttribute('x2', String(width));
  zero.setAttribute('y1', String(y(0)));
  zero.setAttribute('y2', String(y(0)));
  svg.appendChild(zero);

  const path = document.createElementNS(ns, 'path');
  const d = points.map(function (point, index) {
    return (index === 0 ? 'M' : 'L') + x(Number(point.ts)).toFixed(1) + ' ' + y(Number(point.premiumBps)).toFixed(1);
  }).join(' ');
  path.setAttribute('d', d);
  svg.appendChild(path);

  host.appendChild(svg);

  const scale = document.createElement('div');
  scale.className = 'note';
  text(scale, num(lowest, 0) + ' bps to ' + num(highest, 0) + ' bps, ' + new Date(first).toLocaleString() + ' to ' + new Date(last).toLocaleString());
  host.appendChild(scale);
}

function describeQuote(body) {
  const box = byId('quote-result');
  box.replaceChildren();
  const list = document.createElement('dl');
  const add = function (label, value) {
    if (!value) return;
    const dt = document.createElement('dt');
    text(dt, label);
    const dd = document.createElement('dd');
    text(dd, value);
    list.appendChild(dt);
    list.appendChild(dd);
  };
  if (body.kind === 'stock') {
    const fair = body.fairValue || {};
    add('Symbol', fair.symbol);
    add('On chain', usd(fair.onchainMid));
    add('Fair value', usd(fair.reference));
    add((body.side === 'sell' ? 'Sell ' : 'Buy ') + body.amountUsd + ' USD at', usd(body.sizedPrice));
    add('Reference', fair.referenceSource);
    add('Premium', bps(fair.premiumBps));
    add('Session', fair.sessionState);
    add('Note', body.note);
  } else {
    const quote = body.quote || {};
    add('Token', quote.symbol || body.token);
    add('Quoted in', quote.stockSymbol);
    add('On chain', usd(quote.usdOnchain));
    add('Fair value', usd(quote.usdFair));
    add('Stock leg premium', bps(quote.stockLegPremiumBps));
    add('Through ether', usd(quote.usdViaWeth));
    add('Best way in', quote.bestEntryRoute ? usd(quote.bestEntryRoute.usdPrice) : '');
    add('Best way out', quote.bestExitRoute ? usd(quote.bestExitRoute.usdPrice) : '');
  }
  box.classList.remove('note');
  box.appendChild(list);
}

function describePlan(plan) {
  const box = byId('hedge-result');
  box.replaceChildren();
  const list = document.createElement('dl');
  const add = function (label, value) {
    if (!value) return;
    const dt = document.createElement('dt');
    text(dt, label);
    const dd = document.createElement('dd');
    text(dd, value);
    list.appendChild(dt);
    list.appendChild(dd);
  };
  add('Symbol', plan.symbol);
  add('Stock exposure', usd(plan.stockLegNotionalUsd));
  add('Reference price', usd(plan.referencePrice));
  add('Short to hold', plan.targetShortBase ? num(plan.targetShortBase.value, 4) : '');
  add('Held now', plan.currentShortBase ? num(plan.currentShortBase.value, 4) : '');
  add('Next step', plan.action);
  add('Funding 8h', bps(plan.fundingBps8h));
  add('Can be sent', plan.executable ? 'yes' : 'no');
  add('Reason', plan.reason);
  box.classList.remove('note');
  box.appendChild(list);
}

async function refresh() {
  setError('');
  const jobs = [loadStatus(), loadPremium(), loadOrders(), loadHedge()];
  const results = await Promise.allSettled(jobs);
  const failed = results.filter(function (result) { return result.status === 'rejected'; });
  if (failed.length) setError(failed[0].reason.message);
}

function connect(value) {
  token = value;
  store.setItem('covenant-desk-token', value);
  show(byId('connect'), false);
  show(byId('desk'), true);
  refresh().catch(function (error) { setError(error.message); });
  loadSparkline().catch(function () {});
  if (timer) clearInterval(timer);
  timer = setInterval(function () { refresh().catch(function (error) { setError(error.message); }); }, REFRESH_MS);
}

byId('connect-form').addEventListener('submit', function (event) {
  event.preventDefault();
  const value = byId('token-input').value.trim();
  if (value) connect(value);
});

byId('quote-form').addEventListener('submit', function (event) {
  event.preventDefault();
  const key = encodeURIComponent(byId('quote-token').value.trim());
  const usdSize = encodeURIComponent(byId('quote-usd').value);
  const side = encodeURIComponent(byId('quote-side').value);
  api('/v1/quote?token=' + key + '&amountUsd=' + usdSize + '&side=' + side)
    .then(describeQuote)
    .catch(function (error) { setError(error.message); });
});

byId('hedge-form').addEventListener('submit', function (event) {
  event.preventDefault();
  const symbol = encodeURIComponent(byId('hedge-symbol').value.trim());
  const size = encodeURIComponent(byId('hedge-usd').value);
  api('/v1/hedge/plan?symbol=' + symbol + '&stockLegUsd=' + size)
    .then(function (body) { describePlan(body.plan || {}); })
    .catch(function (error) { setError(error.message); });
});

byId('hedge-unwind').addEventListener('click', function () {
  const symbol = byId('hedge-symbol').value.trim();
  api('/v1/hedge/unwind', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ symbol: symbol || undefined }),
  })
    .then(function (body) {
      const plans = body.plans || [];
      const refused = plans.filter(function (plan) { return plan.executable !== true; });
      if (plans.length === 0) setError('There was nothing to unwind.');
      else if (refused.length > 0) {
        setError(refused.map(function (plan) {
          return plan.symbol + ' was not closed: ' + (plan.reason || 'the venue did not accept the order');
        }).join(' '));
      }
      return loadHedge();
    })
    .catch(function (error) { setError(error.message); });
});

byId('recorder-load').addEventListener('click', function () {
  loadSparkline().catch(function (error) { setError(error.message); });
});

byId('recorder-toggle').addEventListener('click', function () {
  const running = byId('recorder-toggle').dataset.running === '1';
  api('/v1/recorder/' + (running ? 'stop' : 'start'), { method: 'POST' })
    .then(loadStatus)
    .catch(function (error) { setError(error.message); });
});

const fromUrl = new URLSearchParams(window.location.search).get('token');
if (fromUrl) {
  window.history.replaceState({}, '', window.location.pathname);
  connect(fromUrl);
} else {
  const saved = store.getItem('covenant-desk-token');
  if (saved) connect(saved);
  else show(byId('connect'), true);
}
`;

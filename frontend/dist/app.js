/* Built by frontend/scripts/build.mjs; edit frontend/src instead. */
export const finite = value => typeof value === 'number' && Number.isFinite(value);
export function power(value) {
  if (!finite(value)) return '—';
  return Math.abs(value) >= 1000 ? `${(value / 1000).toFixed(2)} kW` : `${Math.round(value)} W`;
}
export function reading(source, now = Date.now()) {
  if (!source) return { value: null, note: 'Unavailable' };
  if (source.status === 'assumed_zero') return { value: 0, note: 'Assumed zero · source unavailable' };
  if (!['valid', 'derived', 'estimated_with_assumed_zero'].includes(source.status)) return { value: null, note: humanStatus(source.status) };
  const value = source.reading?.value;
  const time = source.reading?.observed_at ?? source.reading?.observed_from;
  const stamp = Date.parse(time);
  if (!Number.isFinite(stamp) || now - stamp > 300000 || stamp - now > 30000) return { value: null, note: 'Report is out of date' };
  return { value: finite(value) ? value : null, note: source.status === 'estimated_with_assumed_zero' ? 'Estimate · solar assumed zero' : source.status === 'derived' ? 'Calculated' : 'Reported by sensor' };
}
export function humanStatus(status) {
  return ({ stale: 'Report is out of date', unpublished: 'Not published yet', not_configured: 'Not configured', unavailable: 'Unavailable', incomplete_sources: 'Some readings unavailable', invalid_balance: 'Inconsistent readings', timestamp_skew: 'Readings not aligned', mapping_unverified: 'Mapping needs confirmation' })[status] ?? 'Unavailable';
}
export function intervals(feed) {
  if (!feed?.data || !Array.isArray(feed.data.intervals)) return [];
  return feed.data.intervals.filter(r => finite(r.value_inc_vat) && Number.isFinite(Date.parse(r.start)) && Date.parse(r.end) - Date.parse(r.start) === 1800000);
}
export function londonDateKey(value) {
  const parts = new Intl.DateTimeFormat('en-GB', { timeZone: 'Europe/London', year: 'numeric', month: '2-digit', day: '2-digit' }).formatToParts(new Date(value));
  const part = type => parts.find(p => p.type === type).value;
  return `${part('year')}-${part('month')}-${part('day')}`;
}
export function dayRates(feed, offset, now = Date.now()) {
  const [year, month, day] = londonDateKey(now).split('-').map(Number);
  const target = new Date(Date.UTC(year, month - 1, day + offset)).toISOString().slice(0, 10);
  return intervals(feed).filter(r => londonDateKey(r.start) === target);
}
export function currentRate(feed, now = Date.now()) {
  return intervals(feed).find(r => Date.parse(r.start) <= now && now < Date.parse(r.end)) ?? null;
}
export function forecast(data, array, horizon, now = Date.now()) {
  const matches = (data?.forecasts ?? []).filter(r => r.array === array && r.horizon === horizon);
  if (matches.length !== 1) return null;
  const r = matches[0];
  const stamp = Date.parse(r.reading?.observed_at);
  const limit = finite(data.max_age_seconds) ? data.max_age_seconds * 1000 : 8100000;
  if (r.status !== 'valid' || !finite(r.reading?.value) || r.reading.value < 0 || !Number.isFinite(stamp) || now - stamp > limit || stamp - now > 30000) return null;
  if (londonDateKey(stamp) !== londonDateKey(now)) return null;
  return r.reading.value;
}
export function forecastTotal(data, horizon, now = Date.now()) {
  const values = ['fit', 'hybrid_1', 'hybrid_2'].map(array => forecast(data, array, horizon, now));
  const ids = (data?.forecasts ?? []).filter(r => r.horizon === horizon).map(r => r.entity_id);
  return values.every(finite) && new Set(ids).size === 3 ? values.reduce((a, b) => a + b, 0) : null;
}
export function batteryView(data, now = Date.now()) {
  if (!data || ![data.soc_percent, data.charge_power_w, data.discharge_power_w].every(finite) || data.soc_percent < 0 || data.soc_percent > 100) return null;
  const fetched = Date.parse(data.freshness?.fetched_at);
  if (!Number.isFinite(fetched) || now - fetched > 300000 || fetched - now > 30000) return null;
  const sources = Object.values(data.freshness?.sources ?? {});
  const recent = source => { const stamp = Date.parse(source?.observed_at); return Number.isFinite(stamp) && now - stamp <= 300000 && stamp - now <= 30000; };
  const batteryRecent = sources.some(recent);
  const activity = data.freshness.activity_source;
  const inverterRecent = data.freshness.policy === 'recent_confirmed_same_inverter_activity'
    && activity?.entity_id === 'sensor.solis_s6_eh1p_active_power'
    && activity.status === 'fresh' && activity.role === 'same_inverter_activity_not_battery_measurement' && recent(activity);
  if (!batteryRecent && !inverterRecent) return null;
  const charge = Math.max(0, data.charge_power_w), discharge = Math.max(0, data.discharge_power_w);
  return { soc: data.soc_percent, charge, discharge, activityOnly: !batteryRecent && inverterRecent, state: charge > 0 && discharge > 0 ? 'Mixed flow' : charge > 0 ? 'Charging' : discharge > 0 ? 'Discharging' : 'Idle', older: sources.some(s => now - Date.parse(s.observed_at) > 300000) };
}
export const londonTime = value => {
  const date = new Date(value);
  return Number.isFinite(date.valueOf()) ? new Intl.DateTimeFormat('en-GB', { timeZone: 'Europe/London', hour: '2-digit', minute: '2-digit' }).format(date) : '—';
};

// Gaps and assumptions must not become lines through invented measurements.
export function historySegments(rows, intervalSeconds = 60) {
  const accepted = new Set(['valid', 'fresh', 'derived', 'older_report_accepted_via_group', 'future_within_tolerance']);
  const segments = []; let segment = []; let previous = null;
  for (const row of rows ?? []) {
    const valid = finite(row.value) && Number.isFinite(row.sampled_at) && accepted.has(row.status);
    if (!valid || previous !== null && row.sampled_at - previous > intervalSeconds * 2) {
      if (segment.length) segments.push(segment); segment = [];
    }
    if (valid) segment.push(row);
    previous = row.sampled_at;
  }
  if (segment.length) segments.push(segment);
  return segments;
}


export function costReading(payload, key, now = Date.now()) {
  const entry = payload?.readings?.[key];
  const fetched = Date.parse(payload?.fetched_at);
  if (!Number.isFinite(fetched) || now - fetched > 180000 || fetched - now > 30000 || !finite(entry?.reading?.value) || !['valid', 'delayed_period'].includes(entry.status)) return null;
  const today = londonDateKey(now);
  if (key.includes('_today_') && entry.period_day !== today) return null;
  return { ...entry, value: entry.reading.value };
}
export function todayCostTotal(payload, now = Date.now()) {
  const electricity = costReading(payload, 'electricity_today_cost', now);
  const gas = costReading(payload, 'gas_today_cost', now);
  return electricity && gas ? electricity.value + gas.value : null;
}

const $ = id => document.getElementById(id);
const put = (id, value) => { const node = $(id); if (node && node.textContent !== String(value)) node.textContent = value; };
const make = (tag, text, className) => { const node = document.createElement(tag); if (text !== undefined) node.textContent = text; if (className) node.className = className; return node; };
const SVG = 'http://www.w3.org/2000/svg';
const svgNode = (tag, attrs = {}) => { const el = document.createElementNS(SVG, tag); for (const [key, value] of Object.entries(attrs)) el.setAttribute(key, value); return el; };
const state = { records: {}, day: 'today', auto: true, busy: false, lastLive: null };
const resources = { costs: 60000, energy: 15000, battery: 15000, prices: 300000, forecasts: 300000, health: 60000, version: Infinity };
const data = key => state.records[key]?.error ? null : state.records[key]?.data;
function pref(key, fallback) { try { return localStorage.getItem(`hems-${key}`) ?? fallback; } catch { return fallback; } }
function save(key, value) { try { localStorage.setItem(`hems-${key}`, value); } catch { /* Display remains usable without storage. */ } }
function batteryRender(now) {
  const b = batteryView(data('battery'), now);
  put('battery-soc', b ? `${Math.round(b.soc)}%` : '—'); put('battery-state', b?.state ?? 'Unavailable');
  put('charge', power(b?.charge)); put('discharge', power(b?.discharge));
  put('battery-quality', b ? b.activityOnly ? 'Battery values have older HA reports; recent AC-power activity from the same inverter confirms group activity, not new battery measurements.' : b.older ? 'Some unchanged values have older reports; recent inverter activity is present.' : 'All battery sources reported recently.' : 'Battery reports unavailable or out of date.');
  put('battery-freshness', b ? b.activityOnly ? 'Same-inverter activity' : b.older ? 'Recent group activity' : 'Recent reports' : 'Unavailable');
  const fill = $('battery-fill'); const height = b ? b.soc * .9 : 0;
  fill.setAttribute('height', height); fill.setAttribute('y', 101 - height);
  put('flow-battery', b ? `${b.state} · ${power(Math.max(b.charge, b.discharge))}` : 'Unavailable');
}
function energyRender(now) {
  const e = data('energy');
  const sourceMap = { fit: 'fit_pv', pv1: 'hybrid_pv_1', pv2: 'hybrid_pv_2', home: 'household', import: 'grid_import', export: 'grid_export', 'net-grid': 'grid_net', 'calculated-load': 'household_ac_balance' };
  for (const [id, source] of Object.entries(sourceMap)) put(id, power(reading(e?.[source], now).value));
  const received = state.records.energy?.lastSuccess;
  const current = received && now - received <= 300000;
  const total = e && current && finite(e.solar_total_w) ? e.solar_total_w : null;
  put('solar', power(total)); put('flow-solar', power(total));
  const assumed = Number(e?.solar_assumed_zero_inputs ?? 0);
  put('solar-quality', !e || !current ? 'Solar data unavailable.' : assumed ? `${assumed} solar ${assumed === 1 ? 'input' : 'inputs'} unavailable · treated as zero by policy.` : e.solar_total_status === 'valid_estimate' ? 'Estimate · FIT AC plus hybrid DC inputs.' : 'Zero fallback · total is not verified.');
  const home = reading(e?.household, now); const net = reading(e?.grid_net, now);
  put('reported-load', e?.household?.entity_id ? power(home.value) : 'Not configured');
  put('home-quality', home.note); put('flow-home', power(home.value));
  put('net-grid', power(net.value)); put('flow-grid', finite(net.value) ? `${power(Math.abs(net.value))} ${net.value < 0 ? 'exporting' : net.value > 0 ? 'importing' : 'balanced'}` : 'Unavailable');
  put('grid-direction', finite(net.value) ? net.value < 0 ? 'Exporting to grid' : net.value > 0 ? 'Importing from grid' : 'Grid balanced' : 'Grid unavailable');
  put('grid-arrow', finite(net.value) ? net.value < 0 ? '→' : net.value > 0 ? '←' : '↔' : '—');
  put('grid-quality', e?.grid_import?.status === 'derived' || e?.grid_export?.status === 'derived' ? 'Directional flow derived from signed net power.' : net.note);
  put('confidence', !e ? 'Live energy readings unavailable.' : `${assumed ? `${assumed} assumed-zero solar inputs. ` : ''}${batteryView(data('battery'), now)?.older ? 'Battery includes older unchanged reports. ' : ''}Source quality is shown below.`);
}
function drawRates(rates, now) {
  const host = $('price-chart'); host.replaceChildren();
  if (!rates.length) { host.append(make('p', 'No published intervals are available for this schedule.', 'chart-empty')); return; }
  const width = 640, height = 228, left = 48, top = 18, plotW = 574, plotH = 158;
  const values = rates.map(r => r.value_inc_vat * 100);
  const min = Math.min(0, ...values), max = Math.max(1, ...values) * 1.12;
  const y = n => top + (max - n) / (max - min) * plotH;
  const chart = svgNode('svg', { viewBox: `0 0 ${width} ${height}`, role: 'img', 'aria-label': `${state.day === 'today' ? 'Today' : 'Tomorrow'}: ${rates.length} published half-hour prices. Exact times and prices are available in the table below.` });
  for (let i = 0; i < 4; i++) { const v = min + (max - min) * i / 3; const lineY = y(v); chart.append(svgNode('line', { x1: left, x2: width - 12, y1: lineY, y2: lineY, class: 'chart-grid' })); const label = svgNode('text', { x: left - 8, y: lineY + 5, 'text-anchor': 'end' }); label.textContent = `${Math.round(v)}p`; chart.append(label); }
  const start = Date.parse(rates[0].start), end = Date.parse(rates.at(-1).end);
  for (const rate of rates) {
    const value = rate.value_inc_vat * 100;
    const x = left + (Date.parse(rate.start) - start) / (end - start) * plotW;
    const barWidth = Math.max(1, 1800000 / (end - start) * plotW - 2);
    const isCurrent = Date.parse(rate.start) <= now && now < Date.parse(rate.end);
    const rect = svgNode('rect', { x, y: Math.min(y(0), y(value)), width: barWidth, height: Math.max(1, Math.abs(y(value) - y(0))), rx: 1, class: isCurrent ? 'bar-current' : value < 0 ? 'bar-negative' : 'bar' });
    const title = svgNode('title'); title.textContent = `${londonTime(rate.start)}–${londonTime(rate.end)}: ${value.toFixed(2)} p/kWh`; rect.append(title); chart.append(rect);
  }
  chart.append(svgNode('line', { x1: left, x2: width - 12, y1: y(0), y2: y(0), class: 'chart-grid' }));
  const marks = [...new Set([0, Math.floor(rates.length / 4), Math.floor(rates.length / 2), Math.floor(rates.length * 3 / 4), rates.length - 1])];
  for (const index of marks) { const x = left + (Date.parse(rates[index].start) - start) / (end - start) * plotW; const label = svgNode('text', { x, y: 207, 'text-anchor': index === rates.length - 1 ? 'end' : 'start' }); label.textContent = londonTime(rates[index].start); chart.append(label); }
  host.append(chart);
}
let ratesSignature = '';
function pricesRender(now) {
  const prices = data('prices'); const feed = state.day === 'today' ? prices : prices?.next_day;
  const current = currentRate(prices, now);
  put('grid-price', current ? `${(current.value_inc_vat * 100).toFixed(2)} p/kWh` : 'Unavailable');
  put('grid-period', current ? `${londonTime(current.start)}–${londonTime(current.end)}` : 'No current interval');
  const rates = dayRates(feed, state.day === 'today' ? 0 : 1, now);
  put('price-value', state.day === 'today' && current ? `${(current.value_inc_vat * 100).toFixed(2)} p/kWh` : state.day === 'tomorrow' && rates.length ? `${Math.min(...rates.map(r => r.value_inc_vat * 100)).toFixed(2)} p/kWh` : 'Unavailable');
  put('price-label', state.day === 'today' ? 'Current rate, including VAT' : 'Lowest published rate, including VAT');
  put('price-period', state.day === 'today' && current ? `${londonTime(current.start)}–${londonTime(current.end)}` : state.day === 'tomorrow' ? 'Tomorrow’s published schedule' : 'Outside published coverage');
  const formatDate = value => new Intl.DateTimeFormat('en-GB', { timeZone: 'Europe/London', day: 'numeric', month: 'short', hour: '2-digit', minute: '2-digit' }).format(new Date(value));
  put('price-coverage', rates.length ? `${rates.length} half-hours · ${formatDate(rates[0].start)} to ${formatDate(rates.at(-1).end)}. Gaps aren’t filled.` : intervals(feed).length ? 'No published intervals for the selected date.' : humanStatus(feed?.status));
  const signature = JSON.stringify([state.day, rates, current?.start]);
  if (signature !== ratesSignature) {
    ratesSignature = signature; drawRates(rates, now); const tbody = $('rates-body'); tbody.replaceChildren();
    for (const r of rates) { const row = make('tr'); if (Date.parse(r.start) <= now && now < Date.parse(r.end)) row.className = 'current'; for (const value of [londonTime(r.start), londonTime(r.end), (r.value_inc_vat * 100).toFixed(2)]) row.append(make('td', value)); tbody.append(row); }
    put('rates-caption', `${state.day === 'today' ? 'Today' : 'Tomorrow'} · published rates · Europe/London`);
  }
}
function forecastsRender(now) {
  const f = data('forecasts'); const host = $('forecast-arrays'); host.replaceChildren();
  for (const [array, label] of [['fit', 'Original FIT (SW)'], ['hybrid_1', 'Hybrid (NW)'], ['hybrid_2', 'Hybrid (W)']]) {
    const card = make('div', undefined, 'forecast-array'); card.append(make('h3', label));
    for (const [horizon, color] of [['today', 'blue'], ['tomorrow', 'green']]) {
      const value = forecast(f, array, horizon, now); const row = make('div', undefined, `forecast-row ${color}`);
      const bar = svgNode('svg', { viewBox: '0 0 50 14', 'aria-hidden': 'true' }); bar.append(svgNode('rect', { width: 50, height: 14, rx: 3, fill: '#29364a' }));
      if (finite(value)) bar.append(svgNode('rect', { width: Math.min(50, value / 10 * 50), height: 14, rx: 3, fill: horizon === 'today' ? '#76aaff' : '#47e28b' }));
      row.append(bar, make('span', finite(value) ? `${value.toFixed(2)} kWh` : 'Unavailable')); row.setAttribute('aria-label', `${horizon}: ${finite(value) ? value.toFixed(2) + ' kilowatt-hours' : 'unavailable'}`); card.append(row);
    }
    host.append(card);
  }
  for (const horizon of ['today', 'tomorrow']) { const total = forecastTotal(f, horizon, now); put(`forecast-${horizon}`, finite(total) ? `${total.toFixed(2)} kWh` : 'Unavailable'); }
  const stamps = (f?.forecasts ?? []).map(r => Date.parse(r.source_timestamps?.observed_at)).filter(Number.isFinite);
  put('forecast-quality', stamps.length ? `Oldest source report: ${londonTime(Math.min(...stamps))}. Stale forecasts are excluded.` : 'Forecast source reports unavailable.');
}
function diagnosticsRender() {
  const host = $('diagnostics-body'); host.replaceChildren(); const e = data('energy');
  const rows = [['FIT solar', e?.fit_pv], ['Hybrid PV 1', e?.hybrid_pv_1], ['Hybrid PV 2', e?.hybrid_pv_2], ['Household load', e?.household], ['Grid net', e?.grid_net], ['Hybrid AC', e?.hybrid_ac]];
  for (const [key, label] of [['soc_percent', 'Battery state of charge'], ['charge_power_w', 'Battery charge'], ['discharge_power_w', 'Battery discharge']]) { const source = data('battery')?.freshness?.sources?.[key]; rows.push([label, source ? { status: source.status, reading: source } : null]); }
  const activity = data('battery')?.freshness?.activity_source;
  if (activity) rows.push(['Hybrid inverter activity (battery group)', { status: activity.status, reading: activity }]);
  for (const [label, source] of rows) {
    const status = source?.status;
    const quality = ({ valid: 'Reported', derived: 'Calculated', fresh: 'Recent report', older_report_accepted_via_group: 'Older report · accepted via group', assumed_zero: `Assumed zero · ${humanStatus(source?.source_status).toLowerCase()}`, estimated_with_assumed_zero: 'Estimate · assumed solar zero', future_within_tolerance: 'Clock ahead · within tolerance' })[status] ?? humanStatus(status);
    const row = make('tr'); row.append(make('th', label), make('td', quality), make('td', londonTime(source?.reading?.observed_at ?? source?.reading?.observed_from))); row.firstChild.scope = 'row'; host.append(row);
  }
}
function render() {
  const now = Date.now(); batteryRender(now); energyRender(now); pricesRender(now); forecastsRender(now); diagnosticsRender(); costsRender(now);
  const health = data('health'); const healthAge = now - (state.records.health?.lastSuccess ?? 0);
  const online = health?.status === 'healthy' && healthAge <= 120000;
  put('health', online ? 'HEMS Online' : 'Connection unavailable'); put('sidebar-status', online ? 'Service online' : 'Service unavailable');
  for (const id of ['health-dot', 'service-dot']) $(id).className = `dot ${online ? 'online' : 'offline'}`;
  put('last-refresh', state.lastLive ? `Last received ${londonTime(state.lastLive)}` : 'Waiting for live data');
  const failed = Object.entries(state.records).filter(([, r]) => r.error).map(([key]) => key);
  const notice = $('notice'); notice.hidden = !failed.length && state.auto;
  notice.textContent = failed.length ? `Unable to refresh ${failed.join(', ')}. Affected readings are unavailable; retry with Refresh.` : 'Automatic refresh is paused. Values will age out; press Refresh for a new reading.';
  put('clock', new Intl.DateTimeFormat('en-GB', { timeZone: 'Europe/London', dateStyle: 'medium', timeStyle: 'short' }).format(now));
  const version = data('version')?.version; put('version', version ? `v${version}` : 'Version unavailable'); put('footer-version', version ? `v${version}` : '');
}
async function fetchResource(key) {
  const controller = new AbortController(); const timeout = setTimeout(() => controller.abort(), 25000);
  try {
    const response = await fetch(`/api/${key}`, { method: 'GET', cache: 'no-store', signal: controller.signal, headers: { Accept: 'application/json' } });
    if (!response.ok) throw new Error('Request unsuccessful');
    const payload = await response.json(); if (!payload || typeof payload !== 'object' || Array.isArray(payload)) throw new Error('Invalid response');
    state.records[key] = { data: payload, lastSuccess: Date.now(), nextDue: Date.now() + resources[key], error: false };
    if (key === 'energy') state.lastLive = Date.now();
  } catch {
    const old = state.records[key]; state.records[key] = { data: null, lastSuccess: old?.lastSuccess, nextDue: Date.now() + Math.min(resources[key], 30000), error: true };
  } finally { clearTimeout(timeout); }
}
async function refresh(force = false) {
  if (state.busy || document.hidden || (!state.auto && !force)) return;
  const keys = Object.keys(resources).filter(key => force || !state.records[key] || state.records[key].nextDue <= Date.now());
  if (!keys.length) return;
  state.busy = true; $('refresh').disabled = true; $('refresh').setAttribute('aria-busy', 'true');
  try { await Promise.allSettled(keys.map(fetchResource)); render(); }
  finally { state.busy = false; $('refresh').disabled = false; $('refresh').setAttribute('aria-busy', 'false'); }
}
state.auto = pref('auto', 'true') !== 'false'; $('auto-refresh').checked = state.auto;
$('large-text').checked = pref('large', 'false') === 'true'; document.documentElement.classList.toggle('large-text', $('large-text').checked);
$('refresh').addEventListener('click', () => refresh(true));
$('auto-refresh').addEventListener('change', event => { state.auto = event.target.checked; save('auto', String(state.auto)); render(); if (state.auto) refresh(true); });
$('large-text').addEventListener('change', event => { document.documentElement.classList.toggle('large-text', event.target.checked); save('large', String(event.target.checked)); });
for (const button of document.querySelectorAll('[data-day]')) button.addEventListener('click', () => { state.day = button.dataset.day; for (const peer of document.querySelectorAll('[data-day]')) peer.setAttribute('aria-pressed', String(peer === button)); pricesRender(Date.now()); });
for (const link of document.querySelectorAll('nav a')) link.addEventListener('click', () => { for (const peer of document.querySelectorAll('nav a')) { peer.classList.toggle('active', peer === link); if (peer === link) peer.setAttribute('aria-current', 'location'); else peer.removeAttribute('aria-current'); } });
document.addEventListener('visibilitychange', () => { if (!document.hidden) { render(); refresh(); } });
setInterval(() => { if (!document.hidden) { render(); refresh(); } }, 15000);
render(); refresh(true);


async function loadHistory() {
  const button = $('history-load'); button.disabled = true;
  put('history-note', 'Loading local history…');
  const controller = new AbortController(); const timeout = setTimeout(() => controller.abort(), 10000);
  const metric = $('history-metric').value;
  const to = Math.floor(Date.now() / 1000), from = to - 86400;
  try {
    const fetchJSON = async path => { const r = await fetch(path, { signal: controller.signal }); if (!r.ok) throw new Error('History unavailable'); return r.json(); };
    const [result, status] = await Promise.all([
      fetchJSON(`/api/history/samples?metric=${encodeURIComponent(metric)}&from=${from}&to=${to}&limit=2000`),
      fetchJSON('/api/history/status')
    ]);
    const rows = result.rows ?? [], segments = historySegments(rows, status.sample_interval_seconds);
    const points = segments.flat(); const host = $('history-chart'); host.replaceChildren();
    if (points.length) {
      const width = 640, height = 230, left = 60, top = 15, plotW = 560, plotH = 165;
      const min = Math.min(0, ...points.map(r => r.value)), max = Math.max(1, ...points.map(r => r.value));
      const x = t => left + (t - from) / (to - from) * plotW;
      const y = v => top + (max - v) / (max - min) * plotH;
      const chart = svgNode('svg', { viewBox: `0 0 ${width} ${height}`, role: 'img', 'aria-label': `${$('history-metric').selectedOptions[0].text}: last 24 hours. Gaps exclude unavailable and assumed-zero readings. Exact recent values are in the table below.` });
      for (const value of [min, (min + max) / 2, max]) {
        chart.append(svgNode('line', { x1: left, x2: left + plotW, y1: y(value), y2: y(value), class: 'chart-grid' }));
        const label = svgNode('text', { x: left - 8, y: y(value) + 5, 'text-anchor': 'end' }); label.textContent = Math.round(value); chart.append(label);
      }
      for (const part of segments) {
        if (part.length === 1) chart.append(svgNode('circle', { cx: x(part[0].sampled_at), cy: y(part[0].value), r: 3, class: 'history-point' }));
        else chart.append(svgNode('polyline', { points: part.map(r => `${x(r.sampled_at)},${y(r.value)}`).join(' '), class: 'history-line' }));
      }
      for (const t of [from, from + 43200, to]) { const label = svgNode('text', { x: x(t), y: 211, 'text-anchor': t === to ? 'end' : 'start' }); label.textContent = londonTime(t * 1000); chart.append(label); }
      host.append(chart);
    } else host.append(make('p', 'No plottable readings yet. Collection runs independently of this dashboard.', 'chart-empty'));
    const tbody = $('history-body'); tbody.replaceChildren();
    for (const r of rows.slice(-120).reverse()) {
      const row = make('tr');
      for (const value of [londonTime(r.sampled_at * 1000), finite(r.value) ? `${r.value.toFixed(1)} ${r.unit ?? ''}` : '—', humanStatus(r.status), londonTime(r.observed_at)]) row.append(make('td', value));
      tbody.append(row);
    }
    put('history-note', `${rows.length} samples in the last 24 hours · ${status.retention_days} day retention · ${status.sample_interval_seconds}s sampling. ${result.truncated ? 'Result truncated; narrow the API time range. ' : ''}Assumed-zero and unavailable readings leave chart gaps. Calculated readings and battery values accepted through group activity retain their quality labels. Loaded ${londonTime(Date.now())}.`);
  } catch {
    $('history-chart').replaceChildren(); $('history-body').replaceChildren();
    put('history-note', 'History could not be loaded. Try again; live readings remain available.');
  } finally { clearTimeout(timeout); button.disabled = false; }
}
$('history-load').addEventListener('click', loadHistory);
$('history-metric').addEventListener('change', loadHistory);


function costsRender(now) {
  const payload = data('costs');
  const money = value => finite(value) ? new Intl.NumberFormat('en-GB', { style: 'currency', currency: 'GBP' }).format(value) : '—';
  const dateLabel = value => {
    if (!/^\d{4}-\d{2}-\d{2}$/.test(value ?? '')) return 'Date unavailable';
    return new Intl.DateTimeFormat('en-GB', { timeZone: 'Europe/London', day: 'numeric', month: 'short' }).format(new Date(`${value}T12:00:00Z`));
  };
  put('today-cost-total', money(todayCostTotal(payload, now)));
  const body = $('costs-body'); body.replaceChildren();
  for (const [fuel, label] of [['electricity', 'Electricity import'], ['gas', 'Gas']]) {
    const cost = costReading(payload, `${fuel}_today_cost`, now), consumption = costReading(payload, `${fuel}_today_consumption`, now), standing = costReading(payload, `${fuel}_standing_charge`, now);
    const row = make('tr'); row.append(make('th', label)); row.firstChild.scope = 'row';
    for (const text of [money(cost?.value), consumption ? `${consumption.value.toFixed(3)} kWh${consumption.source_metadata?.is_estimated ? ' (estimated)' : ''}` : '—', money(standing?.value)]) row.append(make('td', text));
    body.append(row);
  }
  const demand = costReading(payload, 'electricity_meter_demand', now);
  put('meter-demand', demand ? `${power(demand.value)} · source report ${londonTime(demand.reading.observed_at)}` : 'Unavailable');
  const previous = $('costs-previous'); previous.replaceChildren();
  for (const [fuel, label] of [['electricity', 'Electricity'], ['gas', 'Gas']]) {
    const cost = costReading(payload, `${fuel}_previous_cost`, now), consumption = costReading(payload, `${fuel}_previous_consumption`, now);
    previous.append(make('p', `${label}: ${cost ? `${money(cost.value)} for ${dateLabel(cost.period_day)}${cost.status === 'delayed_period' ? ' · delayed report' : ''}` : 'Cost unavailable'}; ${consumption ? `${consumption.value.toFixed(3)} kWh for ${dateLabel(consumption.period_day)}${consumption.source_metadata?.is_estimated ? ' · estimated' : ''}` : 'consumption unavailable'}.`));
  }
  const reports = $('costs-reports'); reports.replaceChildren();
  for (const [key, entry] of Object.entries(payload?.readings ?? {})) {
    const day = entry.period_day ? ` · covers ${dateLabel(entry.period_day)}` : '';
    const row = make('tr');
    const stamp = Date.parse(entry.source_timestamps?.observed_at);
    const report = Number.isFinite(stamp) ? new Intl.DateTimeFormat('en-GB', { timeZone: 'Europe/London', dateStyle: 'short', timeStyle: 'short' }).format(stamp) : '—';
    for (const text of [key.replaceAll('_', ' '), `${entry.status?.replaceAll('_', ' ') ?? 'Unavailable'}${day}`, report]) row.append(make('td', text));
    reports.append(row);
  }
  put('costs-quality', payload ? 'Totals include standing charges already. Both fuels must report today before a combined total is shown. Reports can be delayed or revised; this is not a final bill.' : 'Cost readings unavailable. Check the local Octopus sensor mappings.');
}

/* Built by frontend/scripts/build.mjs; edit frontend/src instead. */
export const finite = value => typeof value === 'number' && Number.isFinite(value);
export function power(value) {
  if (!finite(value)) return '—';
  return Math.abs(value) >= 1000 ? `${(value / 1000).toFixed(2)} kW` : `${Math.round(value)} W`;
}
export function reading(source, now = Date.now()) {
  if (!source) return { value: null, note: 'Unavailable' };
  if (source.status === 'assumed_zero') return { value: 0, note: 'Assumed zero · source unavailable' };
  if (source.status === 'older_report_accepted_via_group') {
    const f = source.freshness, activity = f?.activity_source;
    const stamp = Date.parse(activity?.observed_at), fetched = Date.parse(f?.fetched_at), observed = Date.parse(source.reading?.observed_at);
    const recent = t => Number.isFinite(t) && now - t <= 300000 && t - now <= 30000;
    const valid = source.entity_id === 'sensor.solis_s6_gr1p_active_power'
      && f?.policy === 'recent_confirmed_fit_inverter_activity'
      && activity?.entity_id === 'sensor.solis_s6_gr1p_dc_voltage_1'
      && activity.status === 'fresh' && activity.role === 'same_fit_inverter_activity_not_ac_measurement'
      && recent(stamp) && recent(fetched) && Number.isFinite(observed) && observed - now <= 30000
      && source.reading?.unit === 'W' && finite(source.reading?.value) && source.reading.value >= 0;
    return { value: valid ? source.reading.value : null, note: valid ? 'Older FIT AC report · accepted using recent activity from the FIT inverter.' : 'FIT activity is unverified or out of date' };
  }
  if (!['valid', 'derived', 'estimated_with_assumed_zero'].includes(source.status)) return { value: null, note: humanStatus(source.status) };
  const value = source.reading?.value;
  const time = source.reading?.observed_at ?? source.reading?.observed_from;
  const stamp = Date.parse(time);
  if (!Number.isFinite(stamp) || now - stamp > 300000 || stamp - now > 30000) return { value: null, note: 'Report is out of date' };
  return { value: finite(value) ? value : null, note: source.status === 'estimated_with_assumed_zero' ? 'Estimate · solar assumed zero' : source.status === 'derived' ? 'Calculated' : 'Reported by sensor' };
}
export function solarEstimate(payload, now = Date.now()) {
  const fetched = Date.parse(payload?.fetched_at);
  if (!finite(payload?.solar_total_w) || !Number.isFinite(fetched) || now - fetched > 300000 || fetched - now > 30000) return null;
  for (const key of ['fit_pv', 'hybrid_pv_1', 'hybrid_pv_2']) {
    if (payload[key]?.status === 'older_report_accepted_via_group' && !finite(reading(payload[key], now).value)) return null;
  }
  return payload.solar_total_w;
}
export function humanStatus(status) {
  return ({ stale: 'Report is out of date', unpublished: 'Not published yet', not_configured: 'Not configured', unavailable: 'Unavailable', incomplete_sources: 'Some readings unavailable', invalid_balance: 'Inconsistent readings', timestamp_skew: 'Readings not aligned', mapping_unverified: 'Mapping needs confirmation' })[status] ?? 'Unavailable';
}
export function intervals(feed) {
  if (!feed?.data || !Array.isArray(feed.data.intervals)) return [];
  return feed.data.intervals.filter(r => finite(r.value_inc_vat) && Number.isFinite(Date.parse(r.start)) && Date.parse(r.end) - Date.parse(r.start) === 1800000);
}
const londonDateFormat = new Intl.DateTimeFormat('en-GB', { timeZone: 'Europe/London', year: 'numeric', month: '2-digit', day: '2-digit' });
export function londonDateKey(value) {
  const parts = londonDateFormat.formatToParts(new Date(value));
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
  if (!finite(data?.soc_percent) || data.soc_percent < 0 || data.soc_percent > 100) return null;
  const fetched = Date.parse(data.freshness?.fetched_at);
  if (!Number.isFinite(fetched) || now - fetched > 300000 || fetched - now > 30000) return null;
  const sources = data.freshness?.sources;
  const socAt = Date.parse(sources?.soc_percent?.observed_at);
  if (!Number.isFinite(socAt) || socAt - now > 30000) return null;
  const currentPower = name => {
    const stamp = Date.parse(sources?.[name]?.observed_at);
    const value = data[name];
    return finite(value) && value >= 0 && Number.isFinite(stamp)
      && now - stamp <= 300000 && stamp - now <= 30000 ? value : null;
  };
  const charge = currentPower('charge_power_w'), discharge = currentPower('discharge_power_w');
  const state = charge > 0 && discharge > 0 ? 'Mixed flow' : charge > 0 ? 'Charging'
    : discharge > 0 ? 'Discharging' : charge === 0 && discharge === 0 ? 'Idle' : 'Power unavailable';
  return { soc: data.soc_percent, charge, discharge, state, socAt,
    lastKnown: now - socAt > 300000, future: socAt > now };
}
export const londonTime = value => {
  const date = new Date(value);
  return Number.isFinite(date.valueOf()) ? new Intl.DateTimeFormat('en-GB', { timeZone: 'Europe/London', hour: '2-digit', minute: '2-digit' }).format(date) : '—';
};

// Gaps and assumptions must not become lines through invented measurements.
export function historySegments(rows, intervalSeconds = 60) {
  const accepted = new Set(['valid', 'fresh', 'derived', 'last_known', 'older_report_accepted_via_group', 'future_within_tolerance']);
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

export function socReport(observed, now) {
  const stamp = new Intl.DateTimeFormat('en-GB', {timeZone: 'Europe/London', dateStyle: 'medium', timeStyle: 'short'}).format(observed);
  const minutes = Math.floor((now - observed) / 60000);
  return `Home Assistant report ${stamp}${observed > now ? ' · HA clock ahead' : ` · ${minutes} min old`}`;
}

export function gridExportView(source, period, now = Date.now()) {
  if (source?.status !== 'valid') return { value: null, note: humanStatus(source?.status) };
  const report = reading(source, now);
  if (report.value === null) return report;
  if (report.value < 0 || source.reading.unit !== 'kWh') return { value: null, note: 'Unavailable' };
  const periodLength = { daily: 10, monthly: 7, yearly: 4 }[period];
  if (periodLength) {
    const reportDate = londonDateKey(source.reading.observed_at).slice(0, periodLength);
    if (reportDate !== londonDateKey(now).slice(0, periodLength)) return { value: null, note: humanStatus('stale') };
  }
  return report;
}


export const make = (tag, text, className) => { const node = document.createElement(tag); if (text !== undefined) node.textContent = text; if (className) node.className = className; return node; };

const reportFormat = new Intl.DateTimeFormat('en-GB', {
  timeZone: 'Europe/London', dateStyle: 'short', timeStyle: 'short',
});

function formatReport(stamp) {
  const at = Date.parse(stamp);
  return Number.isFinite(at) ? reportFormat.format(at) : 'Unavailable';
}

function disclosure(label) {
  const details = make('details');
  details.append(make('summary', label));
  return details;
}

function scrollTable(table) {
  const region = make('div', undefined, 'table-scroll');
  region.tabIndex = 0;
  region.setAttribute('role', 'region');
  region.setAttribute('aria-label', table.firstChild.textContent);
  region.append(table);
  return region;
}

function modellingTable(caption, headings, rows) {
  const table = make('table');
  table.append(make('caption', caption));
  const head = make('thead');
  const heading = make('tr');
  for (const label of headings) {
    const cell = make('th', label);
    cell.scope = 'col';
    heading.append(cell);
  }
  head.append(heading);
  table.append(head);
  const body = make('tbody');
  for (const [label, ...values] of rows) {
    const row = make('tr');
    const cell = make('th', label);
    cell.scope = 'row';
    row.append(cell, ...values.map(value => make('td', value)));
    body.append(row);
  }
  table.append(body);
  return scrollTable(table);
}

const localSlotLabel = stamp => stamp.replace('T', ' ').replace(/([+-]\d{2}:\d{2})$/, ' $1');

function slotDuration(row) {
  const seconds = (Date.parse(row.end) - Date.parse(row.start)) / 1000;
  const wholeMinutes = Math.floor(seconds / 60);
  const remainder = seconds % 60;
  return remainder ? `${wholeMinutes}m ${remainder}s` : `${wholeMinutes}m`;
}

function renderProjection(host, p) {
  const quantity = (value, unit, digits = 2) => Number.isFinite(value) ? `${value.toFixed(digits)} ${unit}` : '—';
  host.append(make('h3', '24-hour solar-aware battery and grid projection'));
  host.append(make('p', 'Read-only self-consumption scenario. Solar follows recently measured half-hour patterns and can differ from tomorrow’s weather.'));
  const source = p.initial_soc_source;
  const observed = Date.parse(source?.observed_at);
  if (Number.isFinite(p.initial_soc_percent)) host.append(make('p', `Starting SOC ${quantity(p.initial_soc_percent, '%', 0)} · Solis inverter · ${source?.status === 'last_known' ? 'last known · ' : ''}${Number.isFinite(observed) ? socReport(observed, Date.parse(p.as_of)) : 'Report time unavailable'}.`));
  if (p.totals) {
   const { grid_import_kwh, grid_export_kwh, end_soc_percent } = p.totals;
   host.append(make('p', `Grid import ${quantity(grid_import_kwh, 'kWh')} · grid export ${quantity(grid_export_kwh, 'kWh')} · ending battery ${quantity(end_soc_percent, '%', 0)}.`));
  } else {
   const missing = p.initial_soc_percent === null
    ? 'A recent collector result with a valid numeric Solis SOC is needed.'
    : 'A gap in demand coverage stops the battery and grid projection.';
   host.append(make('p', `${p.available_intervals} of ${p.intervals.length} intervals have battery and grid estimates. ${missing} 24-hour totals are unavailable.`));
  }
  if (p.intervals_with_baseload_fallback) host.append(make('p', `The configured essential baseload fills ${p.intervals_with_baseload_fallback} demand gaps. These intervals may understate household demand.`));
  if (p.intervals_with_nighttime_zero) host.append(make('p', `${p.intervals_with_nighttime_zero} intervals have zero solar because they fall entirely below civil twilight at the site.`));
  if (p.nighttime_location_configured === false) host.append(make('p', 'Site coordinates are needed to identify nighttime solar zeros.'));
  if (p.intervals_without_full_solar_profile) host.append(make('p', `In ${p.intervals_without_full_solar_profile} of ${p.intervals.length} intervals, at least one solar profile is missing. The scenario gives missing inputs no solar credit.`));
  const details = disclosure('Solar-aware half-hour estimates');
  const input = (value, days) => Number.isFinite(value)
   ? `${quantity(value, 'kWh')} (${days} days)`
   : 'Unavailable';
  details.append(modellingTable(
   '24-hour solar-aware scenario · Europe/London · energy per interval',
   ['Start', 'Duration', 'Demand AC', 'FIT AC', 'Hybrid solar DC', 'Battery at end', 'Grid import AC', 'Grid export AC'],
   p.intervals.map(r => [
    localSlotLabel(r.local_start),
    slotDuration(r),
    r.demand_baseload_fallback ? `${quantity(r.demand_ac_kwh, 'kWh')} · baseload` : `${input(r.demand_ac_kwh, r.demand_days)}${r.demand_occupancy_pooled ? ' · occupancy pooled' : ''}`,
    r.solar_nighttime_zero ? '0 kWh · night assumption' : input(r.fit_ac_kwh, r.fit_days),
    r.solar_nighttime_zero ? '0 kWh · night assumption' : input(r.hybrid_dc_kwh, r.hybrid_days),
    quantity(r.soc_end_percent, '%', 0), quantity(r.flow?.grid_import_kwh, 'kWh'), quantity(r.flow?.grid_export_kwh, 'kWh'),
   ]),
  ));
  host.append(details);
  const methods = disclosure('Projection assumptions and replay accuracy');
  const a = p.assumptions; const c = a.conversion_and_limits;
  methods.append(make('p', `Assumed ${quantity(a.usable_kwh, 'kWh', 3)} usable capacity and ${quantity(a.reserve_percent, '%', 0)} reserve. Charge limit ${quantity(c.max_charge_stored_kw, 'kW')} into storage. Discharge limit ${quantity(c.max_discharge_ac_kw, 'kW')} at the AC output.`));
  const efficiency = value => quantity(value * 100, '%', 0);
  methods.append(make('p', `Assumed conversion efficiencies · hybrid DC to AC ${efficiency(c.hybrid_dc_to_ac)} · hybrid DC to storage ${efficiency(c.pv_dc_to_stored)} · FIT AC to storage ${efficiency(c.fit_ac_to_stored)} · storage to AC ${efficiency(c.stored_to_ac)}.`));
  for (const key of ['dispatch', 'solar', 'demand', 'quality', 'backtest']) methods.append(make('p', a[key]));
  methods.append(make('p', `Demand history used ${p.quality.demand_ac_balance_samples.toLocaleString()} AC balance samples and ${p.quality.demand_direct_positive_samples.toLocaleString()} positive direct readings.`));
  for (const r of p.backtest) {
   const score = (s, unit) => `${quantity(s.mean_absolute_error, unit)} (${s.paired_intervals}/${r.intervals} paired)`;
   methods.append(make('p', `${formatReport(r.as_of)} · mean absolute errors for demand ${score(r.demand_ac_kwh, 'kWh')}, grid import ${score(r.grid_import_kwh, 'kWh')}, grid export ${score(r.grid_export_kwh, 'kWh')} and battery SOC ${score(r.soc_percentage_points, 'percentage points')}.`));
  }
  host.append(methods);
}


const quantity = (value, unit) => Number.isFinite(value)
  ? `${value.toFixed(unit === 'W' || unit === '%' ? 0 : 1)} ${unit}`
  : 'Unavailable';

const readable = value => value.replaceAll('_', ' ');

export function renderModelling(host, model) {
  if (model.projection) renderProjection(host, model.projection);

  const demand = disclosure('Demand outlook and demand-only SOC');
  demand.append(make('p', model.diagnostics.soc_scenario));
  demand.append(modellingTable(
    'Demand-only scenario · Europe/London',
    ['Start', 'Demand', 'Descriptive range', 'Demand-only SOC at end'],
    model.predictions.map(p => [
      localSlotLabel(p.local_start),
      quantity(p.demand_w, 'W'),
      Number.isFinite(p.low_w) ? `${quantity(p.low_w, 'W')} – ${quantity(p.high_w, 'W')}` : 'Unavailable',
      quantity(p.soc_scenario_end_percent, '%'),
    ]),
  ));
  host.append(demand);

  const comparisons = model.solar_comparisons;
  const paired = comparisons.filter(s => Number.isFinite(s.error_actual_minus_forecast_kwh)).length;
  const actuals = comparisons.filter(s => Number.isFinite(s.actual_estimate_kwh)).length;
  const solar = disclosure(`Solar forecast accuracy · ${paired} of ${comparisons.length} paired`);
  solar.append(make('p', `${actuals} array-days have enough telemetry for an actual estimate.`));
  if (comparisons.some(s => s.coverage_basis === 'full_day')) {
    solar.append(make('p', 'Without site coordinates, comparisons require coverage of the full local day.'));
  }
  solar.append(modellingTable(
    'Daily solar comparisons · Europe/London',
    ['Day and array', 'Forecast', 'Actual estimate', 'Actual − forecast', 'Coverage', 'Forecast first seen', 'Provider issued'],
    comparisons.map(s => [
      `${s.day} · ${s.array === 'fit' ? 'FIT' : readable(s.array).replace('hybrid', 'Hybrid')} · ${s.basis}`,
      quantity(s.forecast?.kwh, 'kWh'),
      quantity(s.actual_estimate_kwh, 'kWh'),
      quantity(s.error_actual_minus_forecast_kwh, 'kWh'),
      s.coverage_basis === 'no_daylight' ? 'No daylight or civil twilight'
        : `${s.coverage_percent}% · ${s.coverage_basis === 'daylight_and_twilight' ? 'daylight and twilight' : 'full day'}`,
      formatReport(s.forecast?.first_seen),
      formatReport(s.forecast?.provider_issued_at),
    ]),
  ));
  if (comparisons.length) solar.append(make('p', comparisons[0].note));
  host.append(solar);

  const d = model.diagnostics;
  const b = model.battery;
  const ready = model.profiles.filter(p => p.ready).length;
  const training = disclosure('Demand training and battery telemetry');
  host.append(make('p', `${d.accepted_samples.toLocaleString('en-GB')} accepted samples · ${ready} hourly demand profiles ready`, 'small muted'));
  training.append(make('p', `Up to ${d.training_days} days of history. Each hourly profile needs ${d.minimum_days_per_hour} dates with ${d.minimum_samples_per_hour} readings each. Essential baseload ${quantity(d.essential_baseload_w, 'W')}.`));
  for (const key of ['occupancy_basis', 'uncertainty', 'missing_data', 'assumptions']) {
    training.append(make('p', d[key]));
  }
  training.append(make('p', `Estimated energy above reserve ${quantity(b.above_reserve_kwh, 'kWh')}. Median active charge ${quantity(b.charge.median_active_w, 'W')} and discharge ${quantity(b.discharge.median_active_w, 'W')}.`));
  training.append(make('p', b.efficiency_reason));
  const counts = make('ul');
  for (const [metric, count] of Object.entries(d.accepted_by_metric)) {
    counts.append(make('li', `${readable(metric)} · ${count.toLocaleString('en-GB')}`));
  }
  training.append(counts);
  training.append(modellingTable(
    'Hourly demand profiles · Europe/London',
    ['Hour', 'Occupancy', 'Median', 'Qualifying days', 'Status'],
    model.profiles.map(p => [
      `${String(p.hour).padStart(2, '0')}:00`, readable(p.occupancy), quantity(p.median_w, 'W'),
      p.days, p.ready ? 'Ready' : 'Insufficient history',
    ]),
  ));
  host.append(training);
}

const $ = id => document.getElementById(id);
const put = (id, value) => { const node = $(id); if (node && node.textContent !== String(value)) node.textContent = value; };
const SVG = 'http://www.w3.org/2000/svg';
const svgNode = (tag, attrs = {}) => { const el = document.createElementNS(SVG, tag); for (const [key, value] of Object.entries(attrs)) el.setAttribute(key, value); return el; };
const state = { records: {}, day: 'today', auto: true, busy: false, lastLive: null };
const resources = { costs: 60000, 'grid-export': 60000, energy: 15000, battery: 15000, prices: 300000, forecasts: 300000, health: 60000, version: Infinity };
const data = key => state.records[key]?.error ? null : state.records[key]?.data;
function pref(key, fallback) { try { return localStorage.getItem(`hems-${key}`) ?? fallback; } catch { return fallback; } }
function save(key, value) { try { localStorage.setItem(`hems-${key}`, value); } catch { /* Display remains usable without storage. */ } }
function batteryRender(now) {
  const b = batteryView(data('battery'), now);
  put('battery-soc', b ? `${Math.round(b.soc)}%` : '—'); put('battery-state', b?.state ?? 'Unavailable');
  put('charge', power(b?.charge)); put('discharge', power(b?.discharge));
  put('battery-quality', b ? `Solis inverter SOC · ${socReport(b.socAt, now)}` : 'Battery SOC unavailable.');
  put('battery-freshness', b ? b.lastKnown ? 'Last-known SOC' : b.future ? 'HA clock ahead' : 'Recent HA report' : 'Unavailable');
  const fill = $('battery-fill'); const height = b ? b.soc * .9 : 0;
  fill.setAttribute('height', height); fill.setAttribute('y', 101 - height);
  const active = b && (b.charge !== null || b.discharge !== null) ? Math.max(b.charge, b.discharge) : null;
  put('flow-battery', b ? `${b.state} · ${power(active)}` : 'Unavailable');
}
function energyRender(now) {
  const e = data('energy');
  const sourceMap = { fit: 'fit_pv', pv1: 'hybrid_pv_1', pv2: 'hybrid_pv_2', home: 'household', import: 'grid_import', export: 'grid_export', 'net-grid': 'grid_net', 'calculated-load': 'household_ac_balance' };
  for (const [id, source] of Object.entries(sourceMap)) put(id, power(reading(e?.[source], now).value));
  const received = state.records.energy?.lastSuccess;
  const total = received && now - received <= 300000 ? solarEstimate(e, now) : null;
  const current = finite(total);
  put('solar', power(total)); put('flow-solar', power(total));
  const assumed = Number(e?.solar_assumed_zero_inputs ?? 0);
  const olderSolar = Number(e?.solar_older_report_inputs ?? 0);
  const fitNote = olderSolar ? ' FIT AC has an older report; recent activity is confirmed from the FIT inverter.' : '';
  put('solar-quality', (!e || !current ? 'Solar data unavailable.' : assumed ? `${assumed} solar ${assumed === 1 ? 'input' : 'inputs'} unavailable · treated as zero by policy.` : ['valid_estimate', 'estimated_with_older_reports'].includes(e.solar_total_status) ? 'Estimate · FIT AC plus hybrid DC inputs.' : 'Zero fallback · total is not verified.') + (current ? fitNote : ''));
  const home = reading(e?.household, now); const net = reading(e?.grid_net, now);
  put('reported-load', e?.household?.entity_id ? power(home.value) : 'Not configured');
  put('home-quality', home.note); put('flow-home', power(home.value));
  put('net-grid', power(net.value)); put('flow-grid', finite(net.value) ? `${power(Math.abs(net.value))} ${net.value < 0 ? 'exporting' : net.value > 0 ? 'importing' : 'balanced'}` : 'Unavailable');
  put('grid-direction', finite(net.value) ? net.value < 0 ? 'Exporting to grid' : net.value > 0 ? 'Importing from grid' : 'Grid balanced' : 'Grid unavailable');
  put('grid-arrow', finite(net.value) ? net.value < 0 ? '→' : net.value > 0 ? '←' : '↔' : '—');
  put('grid-quality', e?.grid_import?.status === 'derived' || e?.grid_export?.status === 'derived' ? 'Directional flow derived from signed net power.' : net.note);
  put('confidence', !e ? 'Live energy readings unavailable.' : `${assumed ? `${assumed} assumed-zero solar inputs. ` : ''}${batteryView(data('battery'), now)?.lastKnown ? 'Battery SOC is last-known. ' : ''}Source quality is shown below.`);
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
const exportPeriods = [['daily', 'Export today'], ['monthly', 'Export this month'], ['yearly', 'Export this year']];
const exportText = report => report.value === null ? report.note : `${report.value.toFixed(2)} kWh`;

function gridExportRender(now) {
  const totals = data('grid-export');
  for (const [period] of exportPeriods) {
    put(`grid-export-${period}`, exportText(gridExportView(totals?.[period], period, now)));
  }
}

function diagnosticsRender(now) {
  const host = $('diagnostics-body'); host.replaceChildren(); const e = data('energy');
  const rows = [['FIT solar', e?.fit_pv], ['Hybrid PV 1', e?.hybrid_pv_1], ['Hybrid PV 2', e?.hybrid_pv_2], ['Household load', e?.household], ['Grid net', e?.grid_net], ['Hybrid AC', e?.hybrid_ac]];
  for (const [key, label] of [['soc_percent', 'Battery state of charge'], ['charge_power_w', 'Battery charge'], ['discharge_power_w', 'Battery discharge']]) { const source = data('battery')?.freshness?.sources?.[key]; rows.push([label, source ? { status: source.status, reading: source } : null]); }
  const fitActivity = e?.fit_pv?.freshness?.activity_source;
  if (fitActivity) rows.push(['FIT inverter activity (older AC report)', { status: fitActivity.status, reading: fitActivity }]);
  const activity = data('battery')?.freshness?.activity_source;
  if (activity) rows.push(['Hybrid inverter activity (battery group)', { status: activity.status, reading: activity }]);
  const totals = data('grid-export');
  for (const [period, label] of [...exportPeriods, ['cumulative', 'Cumulative export']]) {
    const source = period === 'cumulative' ? totals?.diagnostics?.cumulative : totals?.[period];
    const report = gridExportView(source, period, now);
    const rowLabel = `Octopus · ${label}`;
    rows.push([rowLabel, source, period === 'cumulative' ? exportText(report) : report.note]);
  }
  for (const [label, source, displayQuality] of rows) {
    const status = source?.status;
    const quality = ({ valid: 'Reported', derived: 'Calculated', fresh: 'Recent report', older_report_accepted_via_group: 'Older report · accepted via group', assumed_zero: `Assumed zero · ${humanStatus(source?.source_status).toLowerCase()}`, estimated_with_assumed_zero: 'Estimate · assumed solar zero', future_within_tolerance: 'Clock ahead · within tolerance' })[status] ?? humanStatus(status);
    const row = make('tr'); row.append(make('th', label), make('td', displayQuality ?? quality), make('td', londonTime(source?.reading?.observed_at ?? source?.reading?.observed_from))); row.firstChild.scope = 'row'; host.append(row);
  }
}
function render() {
  const now = Date.now(); batteryRender(now); energyRender(now); gridExportRender(now); pricesRender(now); forecastsRender(now); diagnosticsRender(now); costsRender(now);
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
    put('history-note', `${rows.length} samples in the last 24 hours · ${status.retention_days} day retention · ${status.sample_interval_seconds}s sampling. ${result.truncated ? 'Result truncated; narrow the API time range. ' : ''}Assumed-zero and unavailable readings leave chart gaps. Calculated readings and last-known battery SOC retain their quality labels. Loaded ${londonTime(Date.now())}.`);
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

async function loadModelling() {
 const button = $('modelling-load'); if (button.disabled || document.hidden) return;
 button.disabled = true; button.setAttribute('aria-busy', 'true');
 const controller = new AbortController(); const timer = setTimeout(() => controller.abort(), 15000);
 put('modelling-status', 'Reading saved history…');
 try {
  const response = await fetch('/api/modelling/summary', { signal: controller.signal });
  if (!response.ok) throw new Error('unavailable');
  const m = await response.json(); const host = $('modelling-result'); host.replaceChildren();
  renderModelling(host, m);
  put('modelling-status', `Saved-history model as of ${londonTime(m.as_of)}. Load again to refresh; results cached for 60 seconds.`);
 } catch { $('modelling-result').replaceChildren(); put('modelling-status', 'Modelling unavailable. Saved history may be busy; try again.'); }
 finally { clearTimeout(timer); button.disabled = false; button.setAttribute('aria-busy', 'false'); }
}
$('modelling-load').addEventListener('click', loadModelling);

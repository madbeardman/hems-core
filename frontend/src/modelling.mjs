import { socReport } from './model.mjs';

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

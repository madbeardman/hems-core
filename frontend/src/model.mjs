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

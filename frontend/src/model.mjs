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

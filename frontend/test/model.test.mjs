import test from 'node:test';
import assert from 'node:assert/strict';
import { reading, power, currentRate, forecast, forecastTotal, batteryView, dayRates } from '../src/model.mjs';
const now = Date.parse('2026-10-28T17:00:00Z');
const stamp = new Date(now).toISOString();
test('an unavailable reading never turns into measured zero', () => {
  assert.equal(reading(null, now).value, null);
  assert.equal(reading({ status: 'stale', reading: { value: 400 } }, now).value, null);
  assert.equal(power(null), '—');
  assert.deepEqual(reading({ status: 'assumed_zero' }, now), { value: 0, note: 'Assumed zero · source unavailable' });
  assert.equal(reading({ status: 'valid', reading: { value: 20, observed_at: stamp } }, now).value, 20);
  assert.equal(reading({ status: 'valid', reading: { value: 20, observed_at: stamp } }, now + 300001).value, null);
});
test('current prices respect exclusive ends, coverage gaps and negative GBP values', () => {
  const r = { start: stamp, end: new Date(now + 1800000).toISOString(), value_inc_vat: -0.05 };
  const feed = { data: { intervals: [r] } };
  assert.equal(currentRate(feed, now).value_inc_vat * 100, -5);
  assert.equal(currentRate(feed, now + 1800000), null);
  assert.equal(currentRate(feed, now - 1), null);
  assert.equal(currentRate(null, now), null);
});
test('forecast totals require three distinct valid sources, without stale or missing data', () => {
  const make = () => ({ max_age_seconds: 8100, forecasts: ['fit', 'hybrid_1', 'hybrid_2'].map((array, i) => ({ array, horizon: 'tomorrow', entity_id: `sensor.fixture_${i}`, status: 'valid', reading: { value: i + 1, observed_at: stamp } })) });
  assert.equal(forecastTotal(make(), 'tomorrow', now), 6);
  assert.equal(forecast(make(), 'fit', 'today', now), null);
  assert.equal(forecastTotal(make(), 'tomorrow', now + 8100001), null);
  const partial = make(); partial.forecasts.pop(); assert.equal(forecastTotal(partial, 'tomorrow', now), null);
  const repeated = make(); repeated.forecasts[2].entity_id = repeated.forecasts[0].entity_id; assert.equal(forecastTotal(repeated, 'tomorrow', now), null);
});
test('battery preserves group acceptance but ages out without a recent report', () => {
  const data = { soc_percent: 87, charge_power_w: 0, discharge_power_w: 265, freshness: { fetched_at: stamp, sources: { soc: { observed_at: stamp }, charge: { observed_at: new Date(now - 7200000).toISOString() } } } };
  assert.equal(batteryView(data, now).state, 'Discharging');
  assert.equal(batteryView(data, now).older, true);
  assert.equal(batteryView(data, now + 300001), null);
  assert.equal(batteryView({ ...data, soc_percent: 101 }, now), null);
});

test('date selectors do not relabel yesterday or today as tomorrow across midnight or DST', () => {
  const rate = start => ({start,end:new Date(Date.parse(start)+1800000).toISOString(),value_inc_vat:.35});
  const feed={data:{intervals:[rate('2026-10-28T23:30:00Z'),rate('2026-10-29T00:00:00Z')]}};
  assert.equal(dayRates(feed,1,Date.parse('2026-10-28T22:00:00Z')).length,1);
  assert.equal(dayRates(feed,1,Date.parse('2026-10-29T00:01:00Z')).length,0);
  // London local date is the 29th before UTC midnight during summer time.
  const summer={data:{intervals:[rate('2026-06-28T23:30:00Z')]}};
  assert.equal(dayRates(summer,0,Date.parse('2026-06-28T23:40:00Z')).length,1);
  assert.equal(dayRates(summer,1,Date.parse('2026-06-28T23:40:00Z')).length,0);
  const missingStamp={status:'valid',reading:{value:42}};
  assert.equal(reading(missingStamp,now).value,null);
});

import test from 'node:test';
import assert from 'node:assert/strict';
import { costReading, todayCostTotal } from '../src/model.mjs';
const now = Date.parse('2026-10-29T12:00:00Z');
const entry = (value, period_day = '2026-10-29', status = 'valid') => ({ reading: { value }, period_day, status });
const payload = readings => ({ fetched_at: new Date(now).toISOString(), readings });
test('today total uses authoritative amounts without adding standing charges again', () => {
  assert.equal(todayCostTotal(payload({ electricity_today_cost: entry(.79), gas_today_cost: entry(.81), electricity_standing_charge: entry(.63), gas_standing_charge: entry(.34) }), now), 1.6);
});
test('missing, delayed or previous-day totals cannot contribute to today', () => {
  for (const gas of [undefined, entry(null), entry(.81, '2026-10-28', 'delayed_period'), entry(.81, '2026-10-28')]) {
    assert.equal(todayCostTotal(payload({ electricity_today_cost: entry(.79), gas_today_cost: gas }), now), null);
  }
});
test('previous periods retain actual dates; expired browser payloads and invalid statuses age out', () => {
  const p = payload({ gas_previous_cost: entry(.8, '2026-10-27', 'delayed_period') });
  assert.equal(costReading(p, 'gas_previous_cost', now).period_day, '2026-10-27');
  assert.equal(costReading(p, 'gas_previous_cost', now + 180001), null);
  assert.equal(costReading(payload({ gas_today_cost: entry(4, '2026-10-29', 'invalid_state') }), 'gas_today_cost', now), null);
});
test('midnight London rollover does not reuse yesterday as today, including BST', () => {
  const t = Date.parse('2026-07-01T22:59:30Z');
  const p = { fetched_at: new Date(t).toISOString(), readings: { gas_today_cost: entry(1, '2026-07-01') } };
  assert.equal(costReading(p, 'gas_today_cost', t).value, 1);
  assert.equal(costReading(p, 'gas_today_cost', t + 60000), null);
});
test('negative electricity costs and signed meter demand are retained; no missing reading becomes zero', () => {
  const p = payload({ electricity_today_cost: entry(-.2), gas_today_cost: entry(0), electricity_meter_demand: entry(-88) });
  assert.equal(todayCostTotal(p, now), -.2);
  assert.equal(costReading(p, 'electricity_meter_demand', now).value, -88);
  assert.equal(costReading(p, 'missing', now), null);
});

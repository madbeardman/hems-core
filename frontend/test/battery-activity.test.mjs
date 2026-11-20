import test from 'node:test';
import assert from 'node:assert/strict';
import { batteryView } from '../src/model.mjs';

const now = Date.parse('2026-10-30T16:06:00Z');
const old = new Date(now - 12 * 3600000).toISOString();
const stamp = new Date(now).toISOString();
const data = () => ({
  soc_percent: 98, charge_power_w: null, discharge_power_w: null,
  freshness: { fetched_at: stamp, sources: { soc_percent: { observed_at: old } } },
});

test('unchanged inverter SOC stays visible while stale power remains unavailable', () => {
  const b = batteryView(data(), now);
  assert.equal(b.soc, 98);
  assert.equal(b.socAt, Date.parse(old));
  assert.equal(b.lastKnown, true);
  assert.equal(b.state, 'Power unavailable');
  assert.equal(batteryView(data(), now + 300001), null);
});

test('charging and discharging do not make SOC recent', () => {
  for (const [direction, state] of [['charge', 'Charging'], ['discharge', 'Discharging']]) {
    const d = data();
    const key = `${direction}_power_w`;
    d[key] = 500;
    d.freshness.sources[key] = { observed_at: stamp };
    const b = batteryView(d, now);
    assert.equal(b.state, state);
    assert.equal(b.lastKnown, true);
  }
});

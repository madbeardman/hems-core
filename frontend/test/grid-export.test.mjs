import test from 'node:test';
import assert from 'node:assert/strict';
import { gridExportView } from '../src/model.mjs';

const now = Date.parse('2026-06-30T23:00:30Z');
const source = (value = 0, stamp = now) => ({
  status: 'valid',
  reading: { value, unit: 'kWh', observed_at: new Date(stamp).toISOString() },
});

test('export totals preserve real zero and expire without a new fetch', () => {
  assert.equal(gridExportView(source(), 'daily', now).value, 0);
  assert.equal(gridExportView(source(8, now - 300001), 'monthly', now).note, 'Report is out of date');
  assert.equal(gridExportView(source(8, now + 30001), 'yearly', now).value, null);
  assert.equal(gridExportView(undefined, 'monthly', now).note, 'Unavailable');
  assert.equal(gridExportView({ ...source(8), status: 'assumed_zero' }, 'daily', now).value, null);
});

test('cached export totals respect London midnight and calendar rollover', () => {
  const beforeLondonMidnight = source(8, now - 31000);
  assert.equal(gridExportView(beforeLondonMidnight, 'daily', now).value, null);
  assert.equal(gridExportView(beforeLondonMidnight, 'monthly', now).value, null);
  assert.equal(gridExportView(beforeLondonMidnight, 'yearly', now).value, 8);
  assert.equal(gridExportView(beforeLondonMidnight, 'cumulative', now).value, 8);
});

import test from 'node:test';
import assert from 'node:assert/strict';
import { historySegments } from '../src/model.mjs';
test('history gaps never join assumed solar zero, stale data or missing samples', () => {
  const row = (sampled_at, status = 'valid', value = 2) => ({ sampled_at, status, value });
  const segments = historySegments([row(0), row(60), row(120, 'assumed_zero', 0), row(180), row(240, 'stale', 5), row(300), row(600), row(660, 'valid', null)]);
  assert.deepEqual(segments.map(s => s.map(r => r.sampled_at)), [[0, 60], [180], [300], [600]]);
});
test('history can show calculated and older group-accepted battery samples without inventing a value', () => {
  assert.equal(historySegments([{ sampled_at: 60, status: 'older_report_accepted_via_group', value: 0 }])[0][0].status, 'older_report_accepted_via_group');
  assert.deepEqual(historySegments([{ sampled_at: 60, status: 'estimated_with_assumed_zero', value: 10 }]), []);
});

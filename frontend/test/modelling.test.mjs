import test from 'node:test';
import assert from 'node:assert/strict';
import { renderModelling } from '../src/modelling.mjs';

class Element {
  constructor(tag) { this.tag = tag; this.children = []; this.attributes = {}; }
  append(...children) { this.children.push(...children); }
  get firstChild() { return this.children[0]; }
  setAttribute(name, value) { this.attributes[name] = value; }
}

function all(node, tag) {
  return [...(node.tag === tag ? [node] : []), ...node.children.flatMap(child => all(child, tag))];
}

test('modelling separates SOC scenarios and exposes labelled, keyboard-scrollable details', t => {
  const original = globalThis.document;
  globalThis.document = { createElement: tag => new Element(tag) };
  t.after(() => { globalThis.document = original; });
  const host = new Element('div');
  const slot = { start: '2026-10-25T00:00:00Z', end: '2026-10-25T00:30:00Z', local_start: '2026-10-25T01:00:00+01:00' };
  const night = { ...slot, solar_nighttime_zero: true, fit_ac_kwh: 0, hybrid_dc_kwh: 0, soc_end_percent: 92 };
  const missing = { ...night, local_start: '2026-10-25T01:00:00+00:00', solar_nighttime_zero: false, fit_ac_kwh: null, hybrid_dc_kwh: 0, hybrid_days: 3 };
  renderModelling(host, {
    projection: {
      as_of: slot.start, initial_soc_percent: 88, initial_soc_source: { status: 'last_known', observed_at: slot.start },
      totals: { grid_import_kwh: 0, grid_export_kwh: 1.25, end_soc_percent: 92 },
      intervals: [night, missing], intervals_with_nighttime_zero: 1, intervals_without_full_solar_profile: 1,
      assumptions: { conversion_and_limits: {} },
      quality: { demand_ac_balance_samples: 0, demand_direct_positive_samples: 0 }, backtest: [],
    },
    predictions: [{ ...slot, soc_scenario_end_percent: 86 }],
    diagnostics: { soc_scenario: 'Demand-only assumptions', accepted_samples: 0, accepted_by_metric: {} },
    battery: { charge: {}, discharge: {} },
    profiles: [],
    solar_comparisons: [
      { day: '2026-10-24', array: 'fit', basis: 'AC output', actual_estimate_kwh: 0, error_actual_minus_forecast_kwh: 0, coverage_percent: 100, coverage_basis: 'daylight_and_twilight' },
      { day: '2026-10-24', array: 'hybrid_1', basis: 'DC input NW', coverage_basis: 'no_daylight' },
    ],
  });
  const sections = all(host, 'details');
  assert.ok(sections.every(section => section.firstChild.tag === 'summary' && !section.open));
  const find = text => sections.find(section => section.firstChild.textContent.startsWith(text));
  const cells = node => all(node, 'td').map(cell => cell.textContent);
  const projection = find('Solar-aware half-hour estimates');
  assert.ok(cells(projection).includes('92 %'));
  assert.ok(cells(projection).includes('0 kWh · night assumption'));
  assert.ok(cells(projection).includes('0.00 kWh (3 days)'));
  assert.ok(cells(projection).includes('Unavailable'));
  assert.ok(all(projection, 'th').some(cell => cell.scope === 'row' && cell.textContent.endsWith('+00:00')));
  assert.ok(all(host, 'p').some(p => p.textContent?.includes('ending battery 92 %')));
  assert.ok(cells(find('Demand outlook')).includes('86 %'));
  assert.ok(cells(find('Solar forecast accuracy · 1 of 2 paired')).includes('No daylight or civil twilight'));
  for (const region of all(host, 'div').filter(node => node.className === 'table-scroll')) {
    assert.equal(region.tabIndex, 0);
    assert.equal(region.attributes.role, 'region');
    assert.equal(region.attributes['aria-label'], all(region, 'caption')[0].textContent);
  }
});

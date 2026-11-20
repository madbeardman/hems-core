import test from 'node:test';
import assert from 'node:assert/strict';
import { reading, solarEstimate } from '../src/model.mjs';
const now=Date.parse('2026-11-01T15:00:00Z'), stamp=new Date(now).toISOString(), old=new Date(now-3600000).toISOString();
const fit=()=>({entity_id:'sensor.solis_s6_gr1p_active_power',status:'older_report_accepted_via_group',reading:{value:20,unit:'W',observed_at:old},freshness:{policy:'recent_confirmed_fit_inverter_activity',fetched_at:stamp,activity_source:{entity_id:'sensor.solis_s6_gr1p_dc_voltage_1',status:'fresh',role:'same_fit_inverter_activity_not_ac_measurement',observed_at:stamp}}});
test('older FIT AC report stays visible with its own recent inverter activity',()=>{
 const f=fit();assert.equal(reading(f,now).value,20);assert.match(reading(f,now).note,/Older FIT/);assert.equal(f.reading.observed_at,old);
 const p={fit_pv:f,solar_total_w:58,fetched_at:stamp};assert.equal(solarEstimate(p,now),58);
});
test('unrelated hybrid evidence, unverified mappings and invalid old FIT readings cannot pass as current',()=>{
 for(const change of [f=>f.freshness.activity_source.entity_id='sensor.solis_s6_eh1p_active_power',f=>f.entity_id='sensor.custom_fit',f=>f.freshness.policy='unknown',f=>f.freshness.activity_source.status='stale',f=>f.freshness.activity_source.role='unknown',f=>f.reading.value=-1,f=>f.reading.value=null,f=>f.reading.unit='kWh',f=>f.freshness.activity_source.observed_at=new Date(now+31000).toISOString()]) {
  const f=fit();change(f);assert.equal(reading(f,now).value,null);
 }
});
test('an expired FIT activity report hides its old value and solar total despite a new HTTP fetch',()=>{
 const f=fit(); f.freshness.fetched_at=new Date(now+300001).toISOString();const p={fit_pv:f,solar_total_w:58,fetched_at:f.freshness.fetched_at};
 assert.equal(reading(f,now+300001).value,null);assert.equal(solarEstimate(p,now+300001),null);
});
test('assumed zero retains its label and is not confused with an older measured FIT value',()=>{
 assert.deepEqual(reading({status:'assumed_zero',source_status:'stale',reading:{value:0}},now),{value:0,note:'Assumed zero · source unavailable'});
 assert.equal(reading({status:'stale',reading:{value:20,unit:'W',observed_at:old}},now).value,null);
});

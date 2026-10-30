import test from 'node:test';
import assert from 'node:assert/strict';
import { batteryView } from '../src/model.mjs';
const now=Date.parse('2026-10-30T16:06:00Z'), old=new Date(now-3600000).toISOString(), stamp=new Date(now).toISOString();
const data=()=>({soc_percent:98,charge_power_w:0,discharge_power_w:0,freshness:{fetched_at:stamp,policy:'recent_confirmed_same_inverter_activity',sources:{soc_percent:{observed_at:old},charge_power_w:{observed_at:old},discharge_power_w:{observed_at:old}},activity_source:{entity_id:'sensor.solis_s6_eh1p_active_power',status:'fresh',role:'same_inverter_activity_not_battery_measurement',observed_at:stamp}}});
test('idle battery stays visible using explicit same-inverter activity while retaining older labels',()=>{
 const b=batteryView(data(),now); assert.equal(b.soc,98);assert.equal(b.state,'Idle');assert.equal(b.older,true);assert.equal(b.activityOnly,true);
 assert.equal(batteryView(data(),now+300001),null);
});
test('recent fetch alone, unrelated activity and bad activity timestamps cannot make old battery current',()=>{
 for(const change of [d=>delete d.freshness.activity_source,d=>d.freshness.activity_source.observed_at=old,d=>d.freshness.activity_source.observed_at=new Date(now+31000).toISOString(),d=>d.freshness.activity_source.entity_id='sensor.unrelated',d=>d.freshness.policy='unknown',d=>d.freshness.activity_source.role='unknown',d=>d.freshness.activity_source.status='stale']){
  const d=data();change(d);assert.equal(batteryView(d,now),null);
 }
});
test('activity expiry hides the battery even when the endpoint was fetched recently',()=>{
 const d=data();d.freshness.fetched_at=new Date(now+300001).toISOString();assert.equal(batteryView(d,now+300001),null);
});

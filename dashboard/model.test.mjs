// ABOUTME: Checks dashboard counts and schema using the real project specification.
// ABOUTME: Rejects invalid updates before the page can present them as progress.
import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { packages, counts, filterTasks, snapshot, milestoneDefinitions } from './model.mjs';
const spec = readFileSync(new URL('../docs/SPEC.md', import.meta.url), 'utf8');
test('specification produces one task for each work package', () => {
  const tasks = packages(spec, { T06: 'done', T08: 'active' });
  assert.equal(tasks[0].id, 'T01'); assert.equal(tasks.at(-1).id, 'T18');
  assert.deepEqual(counts(tasks), { done: 1, active: 1, planned: 16, blocked: 0 });
  assert.deepEqual(filterTasks(tasks, 'active').map(task => task.id), ['T08']);
  assert.equal(filterTasks(tasks, 'all').length, 18);
});
test('unknown states, filters, and specification drift reject', () => {
  assert.throws(() => packages(spec, { T08: 'almost' }));
  assert.throws(() => packages(spec, { T99: 'done' }));
  assert.throws(() => packages('', {}));
  assert.throws(() => filterTasks([], 'almost'));
});
test('a live update must contain a valid and unique work map', () => {
  const value = {schema_version:1,headline:'Building',updated_at:new Date().toISOString(),tasks:packages(spec,{}),checks:[],active:[],verified:[],pending:[],activity:[],milestones:[]};
  assert.equal(snapshot(value), value);
  assert.throws(() => snapshot({...value,updated_at:'unknown'}));
  const duplicate = structuredClone(value); duplicate.tasks[1].id = 'T01';
  assert.throws(() => snapshot(duplicate));
});
test('milestone titles and exit evidence come from the real specification', () => {
  const gates = milestoneDefinitions(spec);
  assert.equal(gates.length, 5);
  assert.equal(gates[0].id, 'M0');
  assert.equal(gates[1].title, 'Local playback foundation');
  assert.match(gates[1].evidence, /PCM preservation/);
});
test('malformed nested live data and incomplete task IDs reject before rendering', () => {
  const value = {schema_version:1,headline:'Building',updated_at:'2026-11-18T05:42:00Z',tasks:packages(spec,{}),checks:[],active:[],verified:[],pending:[],activity:[],milestones:[]};
  assert.throws(() => snapshot({...value, active:[null]}));
  assert.throws(() => snapshot({...value, checks:[{title:12, detail:'bad'}]}));
  assert.throws(() => snapshot({...value, milestones:[{id:'M0',title:'Contracts',state:'complete'}]}));
  const bad = structuredClone(value); bad.tasks[17].id = 'T99';
  assert.throws(() => snapshot(bad));
});
test('malformed rendered fields reject while the previously accepted snapshot remains usable', () => {
  const value = {schema_version:1,headline:'Building',summary:'Verified work',focus:'Local playback',updated_at:'2026-11-18T05:42:00Z',source_digest:'a'.repeat(64),requirement_count:69,tasks:packages(spec,{}),checks:[],active:[],verified:[],pending:[],activity:[],milestones:[]};
  let held = snapshot(value);
  for (const patch of [
    {source_digest:42}, {source_digest:'short'}, {requirement_count:'69'}, {requirement_count:-1},
    {summary:{}}, {focus:[]}, {updated_at:1731900000000},
    {milestones:[{id:'M0',title:'Contracts',detail:'Open',state:'active',evidence:12}]},
    {milestones:[{id:['M0'],title:'Contracts',detail:'Open',state:'active',evidence:'Pending qualification'}]},
    {tasks:[{...value.tasks[0],id:['T01']},...value.tasks.slice(1)]},
  ]) {
    assert.throws(() => { held = snapshot({...value,...patch}); });
    assert.equal(held,value);
    assert.equal(held.source_digest.slice(0,12),'aaaaaaaaaaaa');
  }
});

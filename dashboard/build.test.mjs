// ABOUTME: Exercises the dashboard builder against real specification and status files.
// ABOUTME: Checks the generated snapshot consumed by the published page.
import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, readFile, mkdir } from 'node:fs/promises';
import { spawnSync } from 'node:child_process';
import { resolve } from 'node:path';

test('builder emits a complete portable site and traceable progress snapshot', async () => {
  await mkdir('target/progress-dashboard/tests', {recursive:true});
  const output = await mkdtemp(resolve('target/progress-dashboard/tests/build-'));
  const result = spawnSync(process.execPath, ['scripts/build-progress.mjs', output], {encoding:'utf8'});
  assert.equal(result.status, 0, result.stderr);
  const progress = JSON.parse(await readFile(`${output}/progress.json`, 'utf8'));
  const status = JSON.parse(await readFile('dashboard/status.json','utf8'));
  assert.equal(progress.tasks.length, 18);
  assert.equal(progress.tasks[7].title, 'Volume policy, negotiation and device evidence');
  assert.equal(progress.tasks[7].status, status.task_states.T08 ?? 'planned');
  assert.equal(progress.tasks[17].status, status.task_states.T18 ?? 'planned');
  assert.equal(progress.milestones[1].state, status.milestone_states.M1.state);
  assert.ok(progress.requirement_count > 0);
  assert.match(progress.source_digest, /^[a-f0-9]{64}$/);
  for (const file of ['index.html','app.mjs','style.css','model.mjs']) assert.ok((await readFile(`${output}/${file}`)).length > 0);
});

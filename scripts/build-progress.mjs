// ABOUTME: Builds the private live progress page from checked-in facts and canonical documents.
// ABOUTME: Keeps generated artifacts and source provenance under ignored target storage.
import { readFile, mkdir, copyFile, writeFile } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import { fileURLToPath } from 'node:url';
import { resolve } from 'node:path';
import { packages, milestoneDefinitions, snapshot } from '../dashboard/model.mjs';

const root = fileURLToPath(new URL('../', import.meta.url));
const destination = resolve(process.argv[2] ?? `${root}/target/progress-dashboard/site`);
const [spec, requirementsText, statusText] = await Promise.all([
  readFile(`${root}/docs/SPEC.md`, 'utf8'),
  readFile(`${root}/docs/requirements.json`, 'utf8'),
  readFile(`${root}/dashboard/status.json`, 'utf8'),
]);
const status = JSON.parse(statusText);
const requirements = JSON.parse(requirementsText);
const { task_states, milestone_states, ...facts } = status;
const progress = snapshot({
  ...facts,
  tasks: packages(spec, task_states),
  milestones: milestoneDefinitions(spec).map(gate => ({...gate,...milestone_states[gate.id]})),
  requirement_count: requirements.requirements.length,
  source_digest: createHash('sha256').update(spec).update(requirementsText).update(statusText).digest('hex'),
});
await mkdir(destination, {recursive:true});
await Promise.all(['index.html','app.mjs','style.css','model.mjs'].map(file => copyFile(`${root}/dashboard/${file}`, `${destination}/${file}`)));
await writeFile(`${destination}/progress.json`, `${JSON.stringify(progress,null,2)}\n`);
console.log(`Built progress dashboard: ${progress.tasks.length} work packages; source ${progress.source_digest.slice(0,12)}`);

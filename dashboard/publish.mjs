// ABOUTME: Publishes the authorized private Earph dashboard and pushes its live snapshot.
// ABOUTME: Checks identity, ownership, visibility, and fresh remote state for every update.
import { readFile, mkdir, writeFile } from 'node:fs/promises';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { randomUUID } from 'node:crypto';
import { isDeepStrictEqual } from 'node:util';
import { snapshot } from './model.mjs';
import { verifyMetadata } from './publication.mjs';

const root = fileURLToPath(new URL('../',import.meta.url));
const host = 'https://artifacts.folk-fujita.ts.net';
const slug = 'earph-progress';
const base = `${host}/api/v1/artifacts/${slug}`;
const args = process.argv.slice(2);
if (args.some(arg => arg !== '--data-only') || args.length > 1) throw new Error('Usage: scripts/publish-progress [--data-only]');
const dataOnly = args.includes('--data-only');
const operation = `${new Date().toISOString().replaceAll(':','-')}-${randomUUID().slice(0,8)}`;
const receipts = `${root}/target/progress-dashboard/receipts/${operation}`;
await mkdir(receipts,{recursive:true,mode:0o700});
async function request(label,url,options = {}, allowMissing = false) {
  const response = await fetch(url,{...options,signal:AbortSignal.timeout(30_000)});
  const body = await response.text();
  await writeFile(`${receipts}/${label}.json`,JSON.stringify({status:response.status,body:JSON.parse(body)},null,2),{mode:0o600});
  if (allowMissing && response.status === 404) return null;
  if (!response.ok) throw new Error(`${label} failed (${response.status}). Inspect the retained operation receipt before retrying.`);
  return JSON.parse(body);
}
function run(command,arguments_) {
  const result = spawnSync(command,arguments_,{cwd:root,encoding:'utf8'});
  if (result.status !== 0) throw new Error(`${command} failed: ${result.stderr || result.error || 'unknown error'}`);
}
try {
  const identity = await request('identity',`${host}/api/v1/whoami`);
  if (typeof identity.login !== 'string' || !identity.login) throw new Error('Host did not identify the configured machine.');
  const previous = await request('before',base,{},true);
  if (previous) verifyMetadata(previous,identity,slug);
  else if (dataOnly) throw new Error('Artifact does not exist. Publish the page before updating its live data.');
  run(process.execPath,['scripts/build-progress.mjs']);
  const progress = snapshot(JSON.parse(await readFile(`${root}/target/progress-dashboard/site/progress.json`,'utf8')));
  let metadata = previous;
  if (!dataOnly) {
    const archive = `${receipts}/site.tar.gz`;
    run('tar',['-czf',archive,'-C',`${root}/target/progress-dashboard/site`,'.']);
    const publication = await request('publish',`${base}?title=Earph%20Progress&visibility=private&kind=page`,{
      method:'PUT',headers:{'Content-Type':'application/gzip'},body:await readFile(archive),
    });
    metadata = await request('after-publish',base);
    verifyMetadata(metadata,identity,slug);
    if (metadata.version !== publication.version) throw new Error('Fresh artifact version differs from the publication receipt.');
  }
  await request('data-write',`${base}/data/progress/current`,{
    method:'PUT',headers:{'Content-Type':'application/json'},body:JSON.stringify(progress),
  });
  const rows = await request('data-read',`${base}/data/progress`);
  if (!isDeepStrictEqual(rows.items?.find(row => row.key === 'current')?.value,progress)) throw new Error('Fresh shared-store read does not match the published progress.');
  const after = await request('after',base);
  verifyMetadata(after,identity,slug);
  if (after.version !== metadata.version) throw new Error('Artifact changed during the data update. Inspect receipts before retrying.');
  const summary = {operation,slug,version:after.version,url:`${host}/p/${slug}/`,data_verified:true,source_digest:progress.source_digest};
  await writeFile(`${receipts}/summary.json`,`${JSON.stringify(summary,null,2)}\n`,{mode:0o600});
  await writeFile(`${root}/target/progress-dashboard/latest-receipt.json`,`${JSON.stringify(summary,null,2)}\n`,{mode:0o600});
  console.log(JSON.stringify(summary));
} catch (error) {
  console.error(`Progress publication stopped: ${error.message}\nOperation: ${operation}. Receipts are retained under target/progress-dashboard/receipts.`);
  process.exitCode = 1;
}

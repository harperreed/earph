// ABOUTME: Verifies the real published dashboard on desktop and mobile Chromium.
// ABOUTME: Can publish a truthful verification result and prove the live socket delivers it.
import assert from 'node:assert/strict';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { spawnSync } from 'node:child_process';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { homedir } from 'node:os';

const { chromium } = await import(pathToFileURL(resolve('target/progress-browser/node_modules/playwright/index.mjs')));
const browser = await chromium.launch({executablePath:process.env.PROGRESS_CHROMIUM ?? resolve(homedir(),'.cache/ms-playwright/chromium-1255/chrome-linux64/chrome'),headless:true});
const facts = JSON.parse(await readFile('dashboard/status.json','utf8'));
const expectedCounts = {all:18,active:0,done:0,planned:18-Object.keys(facts.task_states).length,blocked:0};
for (const state of Object.values(facts.task_states)) expectedCounts[state]++;
const errors = [];
const log = [];
await mkdir('target/progress-dashboard/browser', {recursive:true});
try {
  const desktop = await browser.newPage({viewport:{width:1440,height:1100}});
  desktop.on('pageerror', error => errors.push(error.message));
  desktop.on('console', message => { if (message.type() === 'error') errors.push(message.text()); });
  let socketCount = 0;
  desktop.on('websocket', () => socketCount++);
  const response = await desktop.goto('https://artifacts.folk-fujita.ts.net/p/earph-progress/');
  assert.equal(response.status(),200,'Published dashboard must exist');
  await desktop.locator('#headline').filter({hasText:facts.headline}).waitFor();
  await desktop.locator('#connection').filter({hasText:'Live connection'}).waitFor();
  await desktop.waitForFunction(() => document.querySelectorAll('.package').length === 18);
  assert.ok(socketCount > 0, 'The page must open its real live WebSocket');
  for (const filter of ['active','done','planned','blocked','all']) {
    await desktop.locator(`[data-filter="${filter}"]`).click();
    assert.equal(await desktop.locator('.package').count(),expectedCounts[filter],`Filter ${filter}`);
  }
  assert.equal(await desktop.locator('.gate.done').count(),Object.values(facts.milestone_states).filter(gate => gate.state === 'done').length,'Milestone claims match recorded evidence');
  await desktop.locator('.gate details summary').first().click();
  assert.match(await desktop.locator('.gate details[open]').innerText(),/Reproducible offline render/);
  await desktop.screenshot({path:'target/progress-dashboard/browser/desktop.png',fullPage:true});
  log.push('PASS desktop layout, 18 packages, all five filters, milestone evidence, live connection');

  const mobile = await browser.newPage({viewport:{width:390,height:844},isMobile:true,hasTouch:true});
  mobile.on('pageerror', error => errors.push(error.message));
  mobile.on('console', message => { if (message.type() === 'error') errors.push(message.text()); });
  await mobile.goto('https://artifacts.folk-fujita.ts.net/p/earph-progress/');
  await mobile.waitForFunction(() => document.querySelectorAll('.package').length === 18);
  assert.equal(await mobile.evaluate(() => document.documentElement.scrollWidth <= innerWidth),true,'Mobile must not overflow');
  await mobile.locator('[data-filter="active"]').click();
  assert.equal(await mobile.locator('.package').count(),expectedCounts.active);
  await mobile.screenshot({path:'target/progress-dashboard/browser/mobile.png',fullPage:true});
  log.push('PASS mobile 390px layout, filter, and no horizontal overflow');

  if (process.argv.includes('--publish-verification')) {
    let navigations = 0;
    desktop.on('framenavigated', frame => { if (frame === desktop.mainFrame()) navigations++; });
    mobile.on('framenavigated', frame => { if (frame === mobile.mainFrame()) navigations++; });
    const file = 'dashboard/status.json';
    const current = JSON.parse(await readFile(file,'utf8'));
    current.updated_at = new Date().toISOString();
    const title = 'Live dashboard checks passed';
    current.activity = [{title,detail:'Published desktop and mobile views, filters, and milestone evidence pass real Chromium checks. The live connection is open.'},...current.activity.filter(item => item.title !== title)].slice(0,5);
    await writeFile(file,`${JSON.stringify(current,null,2)}\n`);
    const publication = spawnSync('scripts/publish-progress',['--data-only'],{encoding:'utf8'});
    assert.equal(publication.status,0,publication.stderr);
    await desktop.waitForFunction(timestamp => document.querySelector('#updated').dataset.timestamp === timestamp,current.updated_at);
    await mobile.waitForFunction(timestamp => document.querySelector('#updated').dataset.timestamp === timestamp,current.updated_at);
    assert.equal(await desktop.locator('#activity-list h3').first().textContent(),title);
    assert.equal(navigations,0,'Live data updates must preserve the open page');
    log.push('PASS real shared-store update reached both open pages over WebSocket without reload');
    log.push(publication.stdout.trim());
  }
  assert.deepEqual(errors,[],'No browser console or uncaught script errors');
  log.push('PASS no browser console errors');
} finally {
  await browser.close();
  await writeFile('target/progress-dashboard/browser/check.log',`${log.join('\n')}\n${errors.length ? errors.join('\n') : ''}`);
}
console.log(log.join('\n'));

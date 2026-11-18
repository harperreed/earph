// ABOUTME: Renders verified progress from a saved snapshot and the host's live data stream.
// ABOUTME: Treats every data field as text and retains the last valid snapshot on errors.
import { snapshot, counts, filterTasks } from './model.mjs';

const $ = id => document.getElementById(id);
let progress;
let filter = 'all';
let connected = false;
let issue = '';
let receivedLive = false;
const date = value => new Date(value).toLocaleString(undefined, {dateStyle:'medium',timeStyle:'short'});
function element(tag, text, className) {
  const node = document.createElement(tag);
  if (text !== undefined) node.textContent = text;
  if (className) node.className = className;
  return node;
}
function status(value) { return element('span', value === 'done' ? 'Scoped done' : value, `status ${value}`); }
function entries(id, items, className, tag = 'article') {
  $(id).replaceChildren(...items.map(item => {
    const node = element(tag, undefined, className);
    node.append(element('h3',item.title), element('p',item.detail));
    return node;
  }));
}
function renderTasks() {
  const tasks = filterTasks(progress.tasks, filter);
  const totals = counts(progress.tasks);
  $('package-count').textContent = `${tasks.length} of ${progress.tasks.length} packages · ${totals.active} active`;
  $('packages').replaceChildren(...tasks.map(task => {
    const card = element('article', undefined, 'package');
    const top = element('div',undefined,'package-top');
    top.append(element('span',task.id,'task-id'), status(task.status));
    card.append(top, element('h3',task.title), element('p',`Requires: ${task.dependencies}`,'dependencies'));
    return card;
  }));
  if (!tasks.length) $('packages').append(element('p','No work packages have this status.','empty'));
}
function health() {
  $('connection').textContent = connected ? 'Live connection' : 'Reconnecting';
  $('connection-dot').className = connected ? 'dot connected' : 'dot';
  if (!progress) return;
  const stale = Date.now() - Date.parse(progress.updated_at) > 30 * 60 * 1000;
  const message = issue || (!connected ? 'Connection interrupted. Showing the last verified snapshot.' : !receivedLive ? 'Showing the published snapshot. Waiting for the live record.' : stale ? 'Live connection · Last verified progress is over 30 minutes old.' : 'Live connection · Updates arrive automatically as work is verified.');
  $('data-notice').textContent = message;
  $('data-notice').classList.toggle('warning', Boolean(issue || !connected || stale));
}
function accept(value, live = false) {
  try {
    const next = snapshot(value);
    if (progress && Date.parse(next.updated_at) < Date.parse(progress.updated_at)) return;
    progress = next;
    receivedLive ||= live;
    issue = '';
    $('headline').textContent = progress.headline;
    $('summary').textContent = progress.summary ?? '';
    $('focus').textContent = progress.focus ?? '';
    entries('checks',progress.checks,'check');
    entries('verified',progress.verified,'evidence-item');
    entries('active',progress.active,'active-item');
    entries('pending',progress.pending,'pending-item');
    entries('activity-list',progress.activity,'','li');
    $('gates').replaceChildren(...progress.milestones.map(gate => {
      const card = element('article',undefined,`gate ${gate.state}`);
      const top = element('div',undefined,'gate-top');
      top.append(element('span',gate.id,'gate-id'),status(gate.state));
      const details = element('details');
      details.append(element('summary','Specification exit evidence'),element('p',gate.evidence));
      card.append(top,element('h3',gate.title),element('p',gate.detail),details);
      return card;
    }));
    $('updated').textContent = `Verified update: ${date(progress.updated_at)}`;
    $('updated').dataset.timestamp = progress.updated_at;
    $('provenance').textContent = `${progress.requirement_count ?? '—'} catalog requirements · source ${progress.source_digest?.slice(0,12) ?? 'unknown'}`;
    renderTasks();
    health();
  } catch {
    issue = 'An invalid progress update was rejected. Showing the last verified snapshot.';
    $('data-notice').textContent = issue;
    health();
  }
}
for (const button of document.querySelectorAll('[data-filter]')) button.addEventListener('click', () => {
  filter = button.dataset.filter;
  for (const other of document.querySelectorAll('[data-filter]')) other.setAttribute('aria-pressed',String(other === button));
  if (progress) renderTasks();
});
fetch('progress.json').then(response => {
  if (!response.ok) throw new Error('Snapshot unavailable');
  return response.json();
}).then(value => accept(value)).catch(() => {
  issue = 'The published snapshot could not load. Waiting for live data.';
  $('data-notice').textContent = issue;
  health();
});
const host = window.artifactHost;
if (host) {
  const stopEvents = host.subscribe(event => {
    if (event.type === 'ready') { connected = true; health(); }
    if (event.type === 'closed') { connected = false; health(); }
    if (event.type === 'version') location.reload();
  });
  const stopData = host.data.watch('progress', rows => {
    const current = rows.find(row => row.key === 'current');
    if (current) accept(current.value,true);
    else { receivedLive = false; health(); }
  });
  const clock = setInterval(health, 60_000);
  window.addEventListener('pagehide', () => { clearInterval(clock); stopData(); stopEvents(); }, {once:true});
} else {
  issue = 'Live updates are unavailable. Showing the published snapshot.';
  health();
}

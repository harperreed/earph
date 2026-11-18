// ABOUTME: Derives dashboard work packages from the authoritative specification.
// ABOUTME: Keeps status counts and filters consistent across all views.
export function packages(specification, states) {
  const tasks = specification.split('\n').filter(line => /^\| T\d{2} \|/.test(line)).map(line => {
    const [id, title, dependencies] = line.split('|').slice(1).map(value => value.trim());
    const status = states[id] ?? 'planned';
    if (!['done', 'active', 'planned', 'blocked'].includes(status)) throw new Error(`Invalid status for ${id}`);
    return { id, title, dependencies, status };
  });
  if (tasks.length !== 18 || new Set(tasks.map(task => task.id)).size !== 18) throw new Error('Expected eighteen unique specification work packages');
  if (Object.keys(states).some(id => !tasks.some(task => task.id === id))) throw new Error('Status refers to an unknown work package');
  return tasks;
}
export function counts(tasks) {
  return tasks.reduce((result, task) => ({ ...result, [task.status]: result[task.status] + 1 }), { done: 0, active: 0, planned: 0, blocked: 0 });
}
export function filterTasks(tasks, filter) {
  if (!['all', 'done', 'active', 'planned', 'blocked'].includes(filter)) throw new Error('Unknown work filter');
  return tasks.filter(task => filter === 'all' || task.status === filter);
}
export function milestoneDefinitions(specification) {
  const gates = specification.split('\n').filter(line => /^\| \*\*M[0-4] — /.test(line)).map(line => {
    const [label, deliverable, evidence] = line.split('|').slice(1).map(value => value.trim());
    const [, id, title] = label.match(/^\*\*(M[0-4]) — (.+)\*\*$/);
    return {id, title, deliverable, evidence};
  });
  if (gates.length !== 5) throw new Error('Expected five specification milestones');
  return gates;
}
export function snapshot(value) {
  if (value?.schema_version !== 1 || !Array.isArray(value.tasks) || value.tasks.length !== 18 || typeof value.headline !== 'string' || typeof value.updated_at !== 'string' || !Number.isFinite(Date.parse(value.updated_at))) throw new Error('Invalid progress snapshot');
  for (const key of ['summary','focus']) if (value[key] !== undefined && typeof value[key] !== 'string') throw new Error(`Invalid ${key}`);
  if (value.source_digest !== undefined && (typeof value.source_digest !== 'string' || !/^[a-f0-9]{64}$/.test(value.source_digest))) throw new Error('Invalid source digest');
  if (value.requirement_count !== undefined && (!Number.isSafeInteger(value.requirement_count) || value.requirement_count < 0)) throw new Error('Invalid requirement count');
  const ids = new Set();
  for (const task of value.tasks) {
    if (!task || typeof task.id !== 'string' || !/^T(0[1-9]|1[0-8])$/.test(task.id) || ids.has(task.id) || typeof task.title !== 'string' || typeof task.dependencies !== 'string' || !['done', 'active', 'planned', 'blocked'].includes(task.status)) throw new Error('Invalid work package');
    ids.add(task.id);
  }
  for (const key of ['checks', 'active', 'verified', 'pending', 'activity', 'milestones']) {
    if (!Array.isArray(value[key])) throw new Error(`Missing ${key}`);
    for (const item of value[key]) {
      if (!item || typeof item.title !== 'string' || typeof item.detail !== 'string') throw new Error(`Invalid ${key} entry`);
      if (key === 'milestones' && (typeof item.id !== 'string' || !/^M[0-4]$/.test(item.id) || !['active','planned','done','blocked'].includes(item.state) || typeof item.evidence !== 'string')) throw new Error('Invalid milestone');
    }
  }
  return value;
}

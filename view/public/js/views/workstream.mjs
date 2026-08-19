/**
 * Work Stream: traceability as a left-to-right river of stage cards
 * (spec: "goal -> spec -> plan -> task/checkpoint -> touched files ->
 * validation"; DESIGN.md §4 band + card system).
 *
 * Every stage is read straight out of the snapshot — artifacts, their parsed
 * attributes, and the structural relationships the producer derived. Nothing is
 * inferred from prose and nothing is filled in when a link is absent: a plan
 * with no goal renders a gap that names the artifact and the missing field,
 * because "work exists without recorded intent" is the finding, not a blank.
 *
 * This view is read-only on purpose. It shows work; it never edits it. The only
 * interactive elements are the shared Inspector and Reader entry points, so
 * Loam View cannot drift into being a second workflow source of truth.
 */

import { inspectButton, maker, readerLink } from './dom.mjs';

/** Plan `Status:` values that count as finished work. Anything else is open. */
const DONE_TASK = /^(done|complete|completed|verified|shipped|merged)\b/i;
/** Lifecycle values that read as settled rather than in flight. */
const SEALED_LIFECYCLE = /^(approved|accepted|achieved|complete|completed|done|sealed|verified|shipped)\b/i;

const STAGE_LABELS = {
  goal: 'Goal',
  spec: 'Specification',
  plan: 'Plan',
  tasks: 'Tasks & checkpoints',
  files: 'Touched files',
  validation: 'Validation',
};

const byPath = (left, right) => (left.path < right.path ? -1 : left.path > right.path ? 1 : 0);
const lifecycleState = (artifact) =>
  SEALED_LIFECYCLE.test(String(artifact.lifecycle_status ?? '')) ? 'sealed' : 'active';

/**
 * Group the snapshot into traceability chains. A plan anchors a chain; goals and
 * specs that no plan claims anchor their own, so unexecuted intent stays visible
 * instead of disappearing because nothing links to it.
 */
export function buildChains(snapshot) {
  const artifacts = snapshot?.artifacts ?? [];
  const relationships = snapshot?.relationships ?? [];
  const byPathIndex = new Map(artifacts.map((artifact) => [artifact.path, artifact]));

  const linked = (from, kind) =>
    byPathIndex.get(relationships.find((edge) => edge.from === from && edge.kind === kind)?.to ?? '') ?? null;

  const codeBySource = new Map();
  for (const artifact of artifacts) {
    if (artifact.kind === 'code' && artifact.attributes?.source_path) {
      codeBySource.set(artifact.attributes.source_path, artifact);
    }
  }

  // Checkpoints name the work they cover through their workstream pointers;
  // there is no checkpoint-to-plan edge in the v1 relationship rules.
  const checkpointsFor = (planPath) => artifacts.filter((artifact) =>
    artifact.kind === 'checkpoint'
    && (artifact.attributes?.workstreams ?? []).some((workstream) =>
      (workstream?.pointers ?? []).includes(planPath)));

  const claimed = new Set();
  const chains = [];

  for (const plan of artifacts.filter((artifact) => artifact.kind === 'plan').sort(byPath)) {
    const attributes = plan.attributes ?? {};
    const spec = linked(plan.path, 'plan-spec') ?? byPathIndex.get(attributes.spec ?? '') ?? null;
    const goal = linked(plan.path, 'plan-goal') ?? byPathIndex.get(attributes.goal ?? '') ?? null;
    if (spec) claimed.add(spec.path);
    if (goal) claimed.add(goal.path);
    claimed.add(plan.path);
    chains.push({
      anchor: plan,
      goal,
      spec,
      plan,
      goalDeclared: attributes.goal ?? null,
      specDeclared: attributes.spec ?? null,
      checkpoints: checkpointsFor(plan.path),
      codeBySource,
    });
  }

  for (const goal of artifacts.filter((artifact) => artifact.kind === 'goal').sort(byPath)) {
    if (claimed.has(goal.path)) continue;
    const spec = linked(goal.path, 'goal-linked-spec');
    if (spec) claimed.add(spec.path);
    chains.push({ anchor: goal, goal, spec, plan: null, checkpoints: [], codeBySource });
  }

  for (const spec of artifacts.filter((artifact) => artifact.kind === 'spec').sort(byPath)) {
    if (claimed.has(spec.path)) continue;
    const goal = linked(spec.path, 'spec-goal') ?? byPathIndex.get(spec.attributes?.goal ?? '') ?? null;
    chains.push({
      anchor: spec,
      goal,
      spec,
      plan: null,
      goalDeclared: spec.attributes?.goal ?? null,
      checkpoints: [],
      codeBySource,
    });
  }

  return chains;
}

/** One provenance stage: the artifact when it exists, the evidence when it does not. */
function provenanceStage(chain, stage, artifact, declared) {
  const anchor = chain.anchor.path;
  const noun = stage === 'goal' ? 'goal' : 'specification';
  if (artifact) {
    return {
      stage,
      state: lifecycleState(artifact),
      title: artifact.title || artifact.path,
      detail: `Recorded in ${artifact.path}.`,
      status: artifact.lifecycle_status || 'linked',
      artifact,
    };
  }
  if (declared) {
    return {
      stage,
      state: 'gap',
      title: `Declared ${noun} is not inventoried`,
      detail: `${anchor} names ${declared}, which this snapshot does not inventory — the link cannot be resolved.`,
      status: 'coverage gap',
    };
  }
  return {
    stage,
    state: 'gap',
    title: `No linked ${noun}`,
    detail: `${anchor} declares no ${noun} and no link points to one, so the work has no recorded ${stage === 'goal' ? 'intent' : 'design'}.`,
    status: 'coverage gap',
  };
}

function planStage(chain) {
  const { plan } = chain;
  if (!plan) {
    return {
      stage: 'plan',
      state: 'gap',
      title: 'No plan',
      detail: `Nothing links an execution plan to ${chain.anchor.path}, so this intent has not been broken into work.`,
      status: 'coverage gap',
    };
  }
  return {
    stage: 'plan',
    state: lifecycleState(plan),
    title: plan.title || plan.path,
    detail: plan.lifecycle_status
      ? `Lifecycle ${plan.lifecycle_status} in ${plan.path}.`
      : `No lifecycle status recorded in ${plan.path}.`,
    status: plan.lifecycle_status || 'in flight',
    artifact: plan,
  };
}

function tasksStage(chain) {
  const { plan } = chain;
  if (!plan) {
    return { stage: 'tasks', state: 'gap', title: 'No tasks', detail: 'Tasks and checkpoints exist only against a plan.', status: 'coverage gap' };
  }
  const attributes = plan.attributes ?? {};
  const statuses = attributes.task_statuses ?? [];
  const observed = attributes.task_count_observed ?? statuses.length;
  const declared = attributes.task_count_declared;
  const done = statuses.filter((status) => DONE_TASK.test(String(status))).length;

  const notes = [];
  if (declared != null && declared !== observed) {
    notes.push(`Plan front matter declares ${declared}.`);
  }
  notes.push(chain.checkpoints.length
    ? 'Checkpoints pointing at this plan:'
    : 'No checkpoint points at this plan, so there is no captured resume state.');

  return {
    stage: 'tasks',
    state: observed === 0 ? 'gap' : done === observed ? 'sealed' : 'active',
    title: observed === 0
      ? 'No task status lines observed'
      : `${done} of ${observed} tasks complete`,
    detail: notes.join(' '),
    status: observed === 0 ? 'coverage gap' : done === observed ? 'complete' : 'in progress',
    items: chain.checkpoints.map((checkpoint) => ({
      artifact: checkpoint,
      label: checkpoint.title || checkpoint.path,
    })),
  };
}

function filesStage(chain) {
  const { plan } = chain;
  if (!plan) {
    return { stage: 'files', state: 'gap', title: 'No touched files', detail: 'Touched files are recorded per plan task.', status: 'coverage gap' };
  }
  const touched = plan.attributes?.touched_files ?? [];
  if (touched.length === 0) {
    return {
      stage: 'files',
      state: 'gap',
      title: 'No touched files recorded',
      detail: `${plan.path} records no task Files: line, so nothing ties this work to code or memory.`,
      status: 'coverage gap',
    };
  }
  return {
    stage: 'files',
    state: 'sealed',
    title: `${touched.length} touched file${touched.length === 1 ? '' : 's'}`,
    detail: 'Declared by the plan. A path with an ingested code page opens that page in Reader; the source file itself stays evidence.',
    status: 'recorded',
    items: touched.map((path) => ({ label: path, artifact: chain.codeBySource.get(path) ?? null, reader: true })),
  };
}

function validationStage(chain) {
  const { plan } = chain;
  if (!plan) {
    return { stage: 'validation', state: 'gap', title: 'No acceptance criteria', detail: 'Validation evidence lives in a plan.', status: 'coverage gap' };
  }
  const { total = 0, done = 0 } = plan.attributes?.acceptance_criteria ?? {};
  if (!total) {
    return {
      stage: 'validation',
      state: 'gap',
      title: 'No acceptance criteria',
      detail: `${plan.path} has no Acceptance criteria checklist, so completion cannot be verified from the plan.`,
      status: 'coverage gap',
    };
  }
  return {
    stage: 'validation',
    state: done === total ? 'sealed' : 'active',
    title: `${done} of ${total} acceptance criteria met`,
    detail: `Counted from the checklist under the Acceptance criteria heading in ${plan.path}.`,
    status: done === total ? 'verified' : 'pending',
  };
}

export function stagesFor(chain) {
  return [
    provenanceStage(chain, 'goal', chain.goal, chain.goalDeclared),
    provenanceStage(chain, 'spec', chain.spec, chain.specDeclared),
    planStage(chain),
    tasksStage(chain),
    filesStage(chain),
    validationStage(chain),
  ];
}

export function initWorkStream({ root = document } = {}) {
  const mount = root.querySelector('[data-mount="work-stream"]');
  if (!mount) return null;
  const doc = mount.ownerDocument;
  const el = maker(doc);

  function renderStage(stage, index) {
    const card = el('article', 'river-stage');
    card.dataset.stage = stage.stage;
    card.dataset.state = stage.state;

    card.appendChild(el('span', 'river-index', stage.state === 'gap' ? '—' : String(index + 1).padStart(2, '0')));
    card.appendChild(el('span', 'river-kind', STAGE_LABELS[stage.stage]));

    if (stage.artifact) {
      card.appendChild(inspectButton(doc, stage.artifact, { label: stage.title, className: 'inspect-link river-title' }));
    } else {
      card.appendChild(el('strong', 'river-title', stage.title));
    }

    card.appendChild(el('p', 'river-detail', stage.detail));

    if (stage.items?.length) {
      const list = el('ul', 'river-items');
      for (const item of stage.items) {
        const row = el('li');
        if (item.reader) {
          // The source path is evidence; only the ingested code page is a document.
          row.appendChild(el('span', 'river-path', item.label));
          if (item.artifact) row.appendChild(readerLink(doc, item.artifact, { label: item.artifact.title || item.artifact.path }));
          else row.appendChild(el('span', 'river-note', 'no code page'));
        } else if (item.artifact) {
          row.appendChild(inspectButton(doc, item.artifact, { label: item.label }));
        } else {
          row.appendChild(el('span', 'river-path', item.label));
        }
        list.appendChild(row);
      }
      card.appendChild(list);
    }

    const status = el('span', 'status-line', stage.status);
    if (stage.state === 'gap') status.classList.add('missing');
    else if (stage.state === 'active') status.classList.add('watch');
    card.appendChild(status);

    if (stage.state === 'sealed') {
      const seal = el('span', 'river-seal', '✓');
      seal.setAttribute('aria-hidden', 'true');
      card.appendChild(seal);
    }
    return card;
  }

  function renderChain(chain) {
    const stages = stagesFor(chain);
    const band = el('section', 'band');
    band.dataset.chain = chain.anchor.path;

    const head = el('header', 'band-head');
    const title = el('h2', 'band-title');
    const handle = el('span', 'drag', '⠿');
    handle.setAttribute('aria-hidden', 'true');
    title.append(handle, doc.createTextNode(chain.anchor.title || chain.anchor.path));
    head.appendChild(title);

    const gaps = stages.filter((stage) => stage.state === 'gap').length;
    const summary = gaps
      ? el('span', 'badge badge-critical', `${gaps} gap${gaps === 1 ? '' : 's'}`)
      : stages.some((stage) => stage.state === 'active')
        ? el('span', 'badge badge-watch', 'active')
        : el('span', 'badge badge-healthy', 'sealed');
    head.appendChild(summary);
    band.appendChild(head);

    const river = el('div', 'river');
    river.setAttribute('role', 'group');
    river.setAttribute('aria-label', `${chain.anchor.title || chain.anchor.path} traceability`);
    stages.forEach((stage, index) => river.appendChild(renderStage(stage, index)));
    band.appendChild(river);
    return band;
  }

  function render(snapshot) {
    mount.replaceChildren();
    const chains = buildChains(snapshot);
    if (!chains.length) {
      const empty = el('p', 'view-empty', 'No goals, specs, or plans are inventoried in this snapshot, so there is no work to trace.');
      empty.dataset.empty = 'work';
      // Honest empty states name the move that would fill them.
      empty.append(doc.createTextNode(' Start one with '), el('code', 'code-chip', '/loam::setting-goals'), doc.createTextNode(', then Refresh.'));
      mount.appendChild(empty);
      return;
    }
    for (const chain of chains) mount.appendChild(renderChain(chain));
  }

  root.addEventListener('loam:render', (event) => render(event.detail?.snapshot));
  return { render };
}

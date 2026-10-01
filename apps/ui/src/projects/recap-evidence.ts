// What a recap clause's receipts point at, beyond the receipts themselves: the sessions, tasks and
// files of the blocks of work whose evidence they share. Pure functions, no React.

import type { FileTouch, FactKind, RecapBlock, Receipt, SessionId, TaskId } from '../data/index.ts';

/** One string per receipt, equal for receipts that point at the same thing. */
export function receiptKey(receipt: Receipt): string {
  switch (receipt.kind) {
    case 'event':
      return `event:${receipt.id}`;
    case 'transcript':
      return `transcript:${receipt.session}:${receipt.offset}`;
    case 'commit':
      return `commit:${receipt.repo}:${receipt.sha}`;
    case 'pull_request':
      return `pull_request:${receipt.url}`;
    case 'job':
      return `job:${receipt.scheduler}:${receipt.id}`;
    case 'file':
      return `file:${receipt.location.machine}:${receipt.location.path}`;
  }
}

export interface Evidence {
  /** Sessions the evidence is in: the matching blocks' sessions, and any transcript receipt's. */
  sessions: SessionId[];
  /** Tasks the matching facts name; failing that, the matching blocks' tasks. */
  tasks: TaskId[];
  /** Files edited, among the matching blocks' files, whose receipts are among the clause's. */
  files: FileTouch[];
}

function factTask(kind: FactKind): TaskId | undefined {
  switch (kind.type) {
    case 'dispatch_started':
    case 'dispatch_finished':
    case 'task_created':
    case 'task_moved':
    case 'task_assigned':
    case 'plan_updated':
    case 'commented':
      return kind.task;
    default:
      return undefined;
  }
}

const sharesAny = (receipts: readonly Receipt[], wanted: ReadonlySet<string>): boolean =>
  receipts.some((r) => wanted.has(receiptKey(r)));

/**
 * The sessions, tasks and files behind `receipts`, from `blocks` (those the clause's paragraph or
 * line covers). A block matches when any of its facts, files, tool runs, turns or line clauses
 * share a receipt with the clause.
 */
export function evidenceFor(receipts: readonly Receipt[], blocks: readonly RecapBlock[]): Evidence {
  const wanted = new Set(receipts.map(receiptKey));
  const sessions = new Set<SessionId>();
  const factTasks = new Set<TaskId>();
  const blockTasks = new Set<TaskId>();
  const files: FileTouch[] = [];
  for (const receipt of receipts) {
    if (receipt.kind === 'transcript') sessions.add(receipt.session);
  }
  for (const { block, line } of blocks) {
    const facts = block.facts.filter((fact) => sharesAny(fact.receipts, wanted));
    const touched = block.files.filter((file) => sharesAny(file.receipts, wanted));
    const matches =
      facts.length > 0 ||
      touched.length > 0 ||
      sharesAny(block.tool_receipts, wanted) ||
      sharesAny(block.turn_receipts, wanted) ||
      line.spans.some((span) => sharesAny(span.receipts, wanted));
    if (!matches) continue;
    if (block.session !== undefined) sessions.add(block.session);
    for (const fact of facts) {
      const task = factTask(fact.kind);
      if (task !== undefined) factTasks.add(task);
    }
    for (const task of block.tasks) blockTasks.add(task);
    for (const file of touched) {
      if (!files.some((f) => f.path === file.path)) files.push(file);
    }
  }
  return {
    sessions: [...sessions],
    tasks: [...(factTasks.size > 0 ? factTasks : blockTasks)],
    files,
  };
}

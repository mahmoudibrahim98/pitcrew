// My tasks: the board filtered to the tasks assigned to me, across every project.

import { useMe } from './data.ts';
import { Board } from './board.tsx';

export function MyTasksPage() {
  const me = useMe();
  return (
    <div className="flex min-w-0 flex-col gap-4 px-6 py-6">
      <h1 className="text-2xl font-semibold">My tasks</h1>
      {me.data !== undefined && <Board assignee={me.data.id} />}
    </div>
  );
}

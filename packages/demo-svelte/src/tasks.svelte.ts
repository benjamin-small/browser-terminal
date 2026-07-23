/**
 * The task store, as a module-level rune.
 *
 * This is the whole contrast with React. `$state` here is a signal that lives
 * *outside* any component, so shell commands can read and mutate it directly:
 * there is no closure to go stale, no mount/unmount lifecycle to track, and
 * nothing to re-register when the data changes. Components that read
 * `store.tasks` re-render because the signal changed, not because we told
 * them to.
 *
 * React's `useState` is scoped to a render, which is why the React demo needs
 * a latest-ref indirection to reach current state. Svelte's equivalent of
 * that indirection is: nothing.
 */
export interface Task {
  id: number;
  title: string;
  done: boolean;
  priority: number;
}

const SEED: Task[] = [
  { id: 1, title: 'wire up the terminal', done: true, priority: 2 },
  { id: 2, title: 'drive Svelte state from a pipeline', done: false, priority: 3 },
  { id: 3, title: 'compare with React', done: false, priority: 1 },
];

// #region store
export const store = $state({
  tasks: SEED,
  nextId: SEED.length + 1,
});

export function addTask(title: string, priority: number, done = false): Task {
  const task: Task = { id: store.nextId++, title, done, priority };
  store.tasks = [...store.tasks, task];
  return task;
}
// #endregion

export function toggleTask(id: number): void {
  store.tasks = store.tasks.map((t) => (t.id === id ? { ...t, done: !t.done } : t));
}

export function removeTask(id: number): void {
  store.tasks = store.tasks.filter((t) => t.id !== id);
}

export function hasTask(id: number): boolean {
  return store.tasks.some((t) => t.id === id);
}

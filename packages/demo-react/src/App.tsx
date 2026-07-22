import { useEffect, useRef, useState } from 'react';
import { BrowserTerminal } from 'browser-terminal';
import { useCommand } from './useTerminal';

export interface Task {
  id: number;
  title: string;
  done: boolean;
  priority: number;
}

const SEED: Task[] = [
  { id: 1, title: 'wire up the terminal', done: true, priority: 2 },
  { id: 2, title: 'drive React state from a pipeline', done: false, priority: 3 },
  { id: 3, title: 'compare with Svelte', done: false, priority: 1 },
];

export function App() {
  const [bt, setBt] = useState<BrowserTerminal | null>(null);
  const [tasks, setTasks] = useState<Task[]>(SEED);
  const nextId = useRef(SEED.length + 1);

  useEffect(() => {
    let live = true;
    let created: BrowserTerminal | null = null;
    BrowserTerminal.create({ globalToggle: true }).then((t) => {
      // StrictMode double-mounts in dev; drop the instance if we already
      // unmounted, or the singleton guard will reject the next create().
      if (!live) {
        t.dispose();
        return;
      }
      created = t;
      setBt(t);
      // For devtools poking and the CI boot check.
      (window as unknown as { bt: BrowserTerminal }).bt = t;
    });
    return () => {
      live = false;
      created?.dispose();
    };
  }, []);

  // --- commands, all reading/writing live component state ---

  useCommand(bt, { name: 'tasks', summary: 'The current task list' }, () => tasks);

  useCommand(
    bt,
    {
      name: 'task add',
      summary: 'Add a task',
      required: [{ name: 'title', shape: 'str' }],
      flags: [{ long: 'priority', short: 'p', shape: 'int', desc: '1-3' }],
    },
    ({ positionals, flags }) => {
      const task: Task = {
        id: nextId.current++,
        title: String(positionals[0]),
        done: false,
        priority: Number(flags.priority ?? 1),
      };
      setTasks((t) => [...t, task]);
      return task;
    },
  );

  useCommand(
    bt,
    { name: 'task done', summary: 'Toggle a task', required: [{ name: 'id', shape: 'int' }] },
    ({ positionals }) => {
      const id = Number(positionals[0]);
      if (!tasks.some((t) => t.id === id)) {
        throw { message: `no task ${id}`, help: 'run `tasks` to see ids' };
      }
      setTasks((t) => t.map((x) => (x.id === id ? { ...x, done: !x.done } : x)));
      return null;
    },
  );

  useCommand(
    bt,
    { name: 'task rm', summary: 'Remove a task', required: [{ name: 'id', shape: 'int' }] },
    ({ positionals }) => {
      const id = Number(positionals[0]);
      setTasks((t) => t.filter((x) => x.id !== id));
      return null;
    },
  );

  // Deliberately WRONG, kept as a teaching aid: registered once with the
  // closure captured at mount, so it always reports the seed list no matter
  // what you add. Compare `tasks` with `tasks-stale` after adding one.
  useEffect(() => {
    if (!bt) return;
    bt.registerCommand(
      { name: 'tasks-stale', summary: 'BROKEN on purpose: stale closure demo' },
      () => tasks,
    );
    return () => bt.unregisterCommand('tasks-stale');
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [bt]); // <- no `tasks` dep: this is the bug being demonstrated

  const remaining = tasks.filter((t) => !t.done).length;

  return (
    <main>
      <h1>
        browser-terminal <span className="x">×</span> React
      </h1>
      <p className="lede">
        Every command below reads and writes this component's <code>useState</code>.
        Type in the terminal panel and watch the list change.
      </p>

      <ul className="tasks">
        {tasks.map((t) => (
          <li key={t.id} className={t.done ? 'done' : ''}>
            <span className="id">#{t.id}</span>
            <span className="title">{t.title}</span>
            <span className={`pri p${t.priority}`}>P{t.priority}</span>
          </li>
        ))}
        {tasks.length === 0 && <li className="empty">no tasks — try `task add 'something'`</li>}
      </ul>
      <p className="count">
        {remaining} remaining / {tasks.length} total
      </p>

      <h2>Try</h2>
      <pre>
{`task add 'ship it' --priority 3
tasks | filter {|t| !$t.done}
tasks | sort-by priority --reverse | head 2
tasks | filter {|t| $t.priority > 1} | map {|t| $t.title}
task done 2

tasks-stale        # broken on purpose — still shows the seed list`}
      </pre>
      <p className="note">
        <code>tasks</code> uses the <code>useCommand</code> latest-ref hook, so it always
        sees current state. <code>tasks-stale</code> was registered the naive way and is
        frozen at mount — the trap the hook exists to solve.
      </p>
    </main>
  );
}

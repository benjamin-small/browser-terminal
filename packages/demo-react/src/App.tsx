import { useEffect, useRef, useState } from 'react';
import { BrowserTerminal } from 'browser-terminal';
import { useCommand } from './useTerminal';
import { ansiToHtml, highlight, region } from './code';
// Shown on the page verbatim, so the example can't drift from the code.
import hookSource from './useTerminal.ts?raw';
import appSource from './App.tsx?raw';

export interface Task {
  id: number;
  title: string;
  done: boolean;
  priority: number;
}

/** Commands whose generated help is shown at the bottom of the page. */
const HELP_FOR = ['task add', 'task done'];

const SEED: Task[] = [
  { id: 1, title: 'wire up the terminal', done: true, priority: 2 },
  { id: 2, title: 'drive React state from a pipeline', done: false, priority: 3 },
  { id: 3, title: 'compare with Svelte', done: false, priority: 1 },
];

function CodeBlock({
  title,
  source,
  name,
  tone,
}: {
  title: string;
  source: string;
  name: string;
  tone?: 'bad';
}) {
  return (
    <details className={tone === 'bad' ? 'code bad' : 'code'} open>
      <summary>{title}</summary>
      <pre>
        <code dangerouslySetInnerHTML={{ __html: highlight(region(source, name)) }} />
      </pre>
    </details>
  );
}

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

  // #region commands
  // `tasks` reads live component state; `task add` writes it. The hook keeps
  // a ref to the latest closure, so these always see current state without
  // re-registering on every render.
  //
  // The `summary` and `desc` strings are the command's documentation:
  // `task add --help` renders them. Nothing registers `--help` — the
  // evaluator intercepts it before binding, so the help page is whatever
  // this signature says.
  useCommand(bt, { name: 'tasks', summary: 'Show the current task list' }, () => tasks);

  useCommand(
    bt,
    {
      name: 'task add',
      summary: 'Add a task to the list',
      required: [{ name: 'title', shape: 'str', desc: 'what needs doing' }],
      flags: [
        { long: 'priority', short: 'p', shape: 'int', desc: 'urgency, 1 (low) to 3 (high)' },
        { long: 'done', short: 'd', desc: 'add it already checked off' },
      ],
    },
    ({ positionals, flags }) => {
      const task: Task = {
        id: nextId.current++,
        title: String(positionals[0]),
        done: Boolean(flags.done),
        priority: Number(flags.priority ?? 1),
      };
      setTasks((t) => [...t, task]);
      return task;
    },
  );
  // #endregion

  useCommand(
    bt,
    {
      name: 'task done',
      summary: 'Check a task off (or back on)',
      required: [{ name: 'id', shape: 'int', desc: 'the id shown in the # column' }],
    },
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
    {
      name: 'task rm',
      summary: 'Remove a task from the list',
      required: [{ name: 'id', shape: 'int', desc: 'the id shown in the # column' }],
    },
    ({ positionals }) => {
      const id = Number(positionals[0]);
      setTasks((t) => t.filter((x) => x.id !== id));
      return null;
    },
  );

  // #region stale
  // BROKEN ON PURPOSE — this is the mistake `useCommand` exists to prevent.
  //
  // The command is registered once, so it captures `tasks` from the render
  // that registered it and reports that forever. Adding a task updates the
  // component but not this closure.
  //
  // Note the eslint-disable: react-hooks/exhaustive-deps *does* catch this,
  // and we had to silence it to keep the bug. Listening to the linter and
  // adding `tasks` to the deps fixes the staleness but re-registers on every
  // render — which is why the hook keeps a ref instead.
  useEffect(() => {
    if (!bt) return;
    bt.registerCommand(
      { name: 'tasks-stale', summary: 'BROKEN on purpose: stale closure demo' },
      () => tasks,
    );
    return () => bt.unregisterCommand('tasks-stale');
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [bt]); // <- no `tasks` dep: this is the bug being demonstrated
  // #endregion

  // #region help
  // Nothing here registers `--help`. Asking the engine for it — rather than
  // pasting the output into the page — means what a visitor reads is
  // generated from the signatures above and can't drift from them.
  //
  // This effect is declared *after* the useCommand calls on purpose: effects
  // in one component run in declaration order, so the commands exist by the
  // time we ask them to describe themselves.
  const [help, setHelp] = useState<string[]>([]);
  useEffect(() => {
    if (!bt) return;
    let live = true;
    Promise.all(HELP_FOR.map((c) => bt.run(`${c} --help`))).then((texts) => {
      if (live) setHelp(texts.map((t) => ansiToHtml(String(t).trimEnd())));
    });
    return () => {
      live = false;
    };
  }, [bt]);
  // #endregion

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
{`task add --help    # every command gets this for free
task add 'ship it' --priority 3
task add 'already did this' --done
tasks | filter {|t| !$t.done}
tasks | sort-by priority --reverse | head 2
tasks | filter {|t| $t.priority > 1} | map {|t| $t.title}
task done 2

tasks-stale        # broken on purpose — still shows the seed list`}
      </pre>
      <h2>How it's implemented</h2>
      <p className="sub">
        Extracted from this page's own source at build time — not a
        transcription.
      </p>
      <CodeBlock title="Registering commands over useState" source={appSource} name="commands" />
      <CodeBlock
        title="✗ The trap: registering the naive way (this is tasks-stale)"
        source={appSource}
        name="stale"
        tone="bad"
      />
      <CodeBlock title="The useCommand hook (latest-ref pattern)" source={hookSource} name="hook" />

      <h2>What that buys you</h2>
      <p className="sub">
        No command registers a <code>--help</code> flag. The evaluator intercepts
        it before argument binding and renders the signature, so these pages come
        from the <code>summary</code> and <code>desc</code> strings above —
        printed by the live engine on this page, not pasted in.
      </p>
      {help.map((html, i) => (
        <details className="code help" open key={HELP_FOR[i]}>
          <summary>{HELP_FOR[i]} --help</summary>
          <pre>
            <code dangerouslySetInnerHTML={{ __html: html }} />
          </pre>
        </details>
      ))}
      <CodeBlock title="Asking the engine for its own help text" source={appSource} name="help" />

      <p className="note">
        <code>tasks</code> uses the <code>useCommand</code> latest-ref hook, so it always
        sees current state. <code>tasks-stale</code> was registered the naive way and is
        frozen at mount — the trap the hook exists to solve.
      </p>
    </main>
  );
}

/**
 * Shell commands over the task store.
 *
 * Note what isn't here: no hooks, no refs, no cleanup, no re-registration.
 * These run once at startup and stay correct forever, because they close over
 * a module-level signal rather than a render-scoped value.
 */
import type { BrowserTerminal } from 'browser-terminal';
import { addTask, hasTask, removeTask, store, toggleTask } from './tasks.svelte';

export function registerTaskCommands(bt: BrowserTerminal): void {
  bt.registerCommand({ name: 'tasks', summary: 'The current task list' }, () => store.tasks);

  bt.registerCommand(
    {
      name: 'task add',
      summary: 'Add a task',
      required: [{ name: 'title', shape: 'str' }],
      flags: [{ long: 'priority', short: 'p', shape: 'int', desc: '1-3' }],
    },
    ({ positionals, flags }) => addTask(String(positionals[0]), Number(flags.priority ?? 1)),
  );

  bt.registerCommand(
    { name: 'task done', summary: 'Toggle a task', required: [{ name: 'id', shape: 'int' }] },
    ({ positionals }) => {
      const id = Number(positionals[0]);
      if (!hasTask(id)) {
        throw { message: `no task ${id}`, help: 'run `tasks` to see ids' };
      }
      toggleTask(id);
      return null;
    },
  );

  bt.registerCommand(
    { name: 'task rm', summary: 'Remove a task', required: [{ name: 'id', shape: 'int' }] },
    ({ positionals }) => {
      removeTask(Number(positionals[0]));
      return null;
    },
  );

  // A `@name` selector function, also just a plain module-scope closure.
  bt.registerFn('slug', (item) =>
    String((item as { title?: string })?.title ?? '')
      .toLowerCase()
      .replace(/[^a-z0-9]+/g, '-')
      .replace(/^-|-$/g, ''),
  );
}

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
  // #region commands
  // No hooks, refs, or cleanup: `store` is a module-level rune, so these
  // closures stay correct for the life of the page.
  //
  // A name with a space in it *is* the subcommand mechanism: registering
  // `task list` and `task add` makes `task` a group, so bare `task` lists
  // them. There is no group object to declare and nothing to attach to.
  //
  // The `summary` and `desc` strings are the command's documentation:
  // `task add --help` renders them. Nothing registers `--help` — the
  // evaluator intercepts it before binding, so the help page is whatever
  // this signature says.
  bt.registerCommand({ name: 'task list', summary: 'Show the current task list' }, () => store.tasks);

  bt.registerCommand(
    {
      name: 'task add',
      summary: 'Add a task to the list',
      required: [{ name: 'title', shape: 'str', desc: 'what needs doing' }],
      flags: [
        { long: 'priority', short: 'p', shape: 'int', desc: 'urgency, 1 (low) to 3 (high)' },
        { long: 'done', short: 'd', desc: 'add it already checked off' },
      ],
    },
    ({ positionals, flags }) =>
      addTask(String(positionals[0]), Number(flags.priority ?? 1), Boolean(flags.done)),
  );
  // #endregion

  bt.registerCommand(
    {
      name: 'task done',
      summary: 'Check a task off (or back on)',
      required: [{ name: 'id', shape: 'int', desc: 'the id shown in the # column' }],
    },
    ({ positionals }) => {
      const id = Number(positionals[0]);
      if (!hasTask(id)) {
        throw { message: `no task ${id}`, help: 'run `task list` to see ids' };
      }
      toggleTask(id);
      return null;
    },
  );

  bt.registerCommand(
    {
      name: 'task rm',
      summary: 'Remove a task from the list',
      required: [{ name: 'id', shape: 'int', desc: 'the id shown in the # column' }],
    },
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

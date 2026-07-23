import { mount } from 'svelte';
import { BrowserTerminal } from 'browser-terminal';
import App from './App.svelte';
import { registerTaskCommands } from './commands';
import { loadHelp } from './help.svelte';

const app = mount(App, { target: document.getElementById('app')! });

// Registered once, at module scope — the commands close over a module-level
// rune, so they stay correct for the life of the page.
const bt = await BrowserTerminal.create({ globalToggle: true });
registerTaskCommands(bt);

// Ask the engine to describe the commands we just registered; the page reads
// the result out of a rune.
await loadHelp(bt, ['task add', 'task done']);

// Handy for poking at it from devtools.
(window as unknown as { bt: BrowserTerminal }).bt = bt;

export default app;

import '@xterm/xterm/css/xterm.css';
import { BrowserTerminal } from 'browser-terminal';

async function main(): Promise<void> {
  await BrowserTerminal.create();
}

void main();

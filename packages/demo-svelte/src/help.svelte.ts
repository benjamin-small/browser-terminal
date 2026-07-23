/**
 * The commands' own `--help` output, as a rune.
 *
 * Nothing registers a `--help` flag: the evaluator intercepts it before
 * argument binding and renders the signature. Asking the live engine for that
 * text — instead of pasting it into the page — means what a visitor reads is
 * generated from the same `summary` and `desc` strings the command was
 * declared with, and can't drift from them.
 *
 * It also happens to be the whole Svelte thesis in miniature: a pipeline runs,
 * writes to a module-level rune, and the component re-renders. No component
 * had to be involved.
 */
import type { BrowserTerminal } from 'browser-terminal';
import { ansiToHtml } from './code';

export interface HelpPage {
  command: string;
  html: string;
}

export const helpPages = $state<HelpPage[]>([]);

/** Run `<command> --help` for each name and store the rendered result. */
export async function loadHelp(bt: BrowserTerminal, commands: string[]): Promise<void> {
  const pages = await Promise.all(
    commands.map(async (command) => ({
      command,
      html: ansiToHtml(String((await bt.run(`${command} --help`)).value).trimEnd()),
    })),
  );
  helpPages.push(...pages);
}

import type { CommandSpec, Shape } from './types.js';

export interface ArgumentCompletionContext {
  command: string;
  args: string[];
  prefix: string;
  argumentIndex: number;
  flag?: string;
  kind: 'argument' | 'redirect';
  session: number;
  pane: number;
  signal: AbortSignal;
}
export interface CompletionItem { value: string; directory?: boolean }
export type CompletionProvider = (context: ArgumentCompletionContext) => Promise<CompletionItem[]> | CompletionItem[];
interface Token { value: string; start: number; quoted: boolean; op?: boolean }

/** Parse only the current top-level command; never evaluate expressions or variables. */
function tokens(line: string): Token[] | null {
  if (/[\x00-\x1f\x7f]/.test(line)) return null;
  let start = 0, quote = '', depth = 0;
  for (let i = 0; i < line.length; i++) {
    const c = line[i]!;
    if (quote) {
      if (quote === '"' && c === '\\') { i++; continue; }
      if (c === quote) quote = '';
    } else if (c === '"' || c === "'") quote = c;
    else if ('{(['.includes(c)) depth++;
    else if ('})]'.includes(c)) depth = Math.max(0, depth - 1);
    else if (!depth && (c === '|' || c === ';')) start = i + 1;
  }
  if (depth) return null;
  const result: Token[] = [];
  let i = start, trailingSpace = false;
  while (i < line.length) {
    if (line[i] === ' ') { i++; trailingSpace = true; continue; }
    trailingSpace = false;
    const token: Token = { value: '', start: i, quoted: false };
    const c = line[i]!;
    if (c === '<' || c === '>') {
      token.op = true; token.value = c; i++;
      if (c === '>' && line[i] === '>') { token.value += '>'; i++; }
    } else if (c === '"' || c === "'") {
      token.quoted = true; i++;
      while (i < line.length && line[i] !== c) {
        if (c === '"' && line[i] === '$') return null;
        if (c === '"' && line[i] === '\\') {
          const escaped = line[++i];
          if (!escaped || !['"', '\\', '$'].includes(escaped)) return null;
        }
        token.value += line[i++];
      }
      if (line[i] === c) i++;
      if (i < line.length && !' <>'.includes(line[i]!)) return null;
    } else {
      while (i < line.length && !' <>'.includes(line[i]!)) {
        if (`$'"{}()[]\\`.includes(line[i]!)) return null;
        token.value += line[i++];
      }
    }
    result.push(token);
  }
  if (!result.length || trailingSpace || result.at(-1)?.op) result.push({ value: '', start: line.length, quoted: false });
  return result;
}

export function argumentTarget(line: string, specs: CommandSpec[]) {
  const words = tokens(line);
  if (!words) return null;
  const current = words.at(-1)!;
  const prior = words.slice(0, -1);
  const spec = [...specs].sort((a, b) => b.name.length - a.name.length).find(s => {
    const name = s.name.split(' ');
    return name.length <= prior.length && name.every((part, i) => prior[i]?.value === part && !prior[i]?.op);
  });
  if (!spec) return null;
  const args = prior.slice(spec.name.split(' ').length);
  const used = new Set<string>();
  let argumentIndex = 0, flag: string | undefined, shape: Shape | undefined, redirect = false;
  for (const token of args) {
    if (redirect) { redirect = false; continue; }
    if (flag) { flag = undefined; shape = undefined; continue; }
    if (token.op) { redirect = true; continue; }
    const name = token.value.split('=')[0];
    const found = !token.quoted && spec.flags?.find(f => name === `--${f.long}` || name === `-${f.short}`);
    if (found) {
      used.add(found.long);
      if (found.shape && !token.value.includes('=')) { flag = found.long; shape = found.shape; }
    } else if (!token.quoted && token.value.startsWith('-')) return null;
    else argumentIndex++;
  }
  let prefix = current.value, replaceStart = current.start;
  if (!current.quoted && prefix.startsWith('--') && prefix.includes('=')) {
    const eq = prefix.indexOf('=');
    const found = spec.flags?.find(f => `--${f.long}` === prefix.slice(0, eq) && f.shape);
    if (!found) return null;
    flag = found.long; shape = found.shape; prefix = prefix.slice(eq + 1); replaceStart += eq + 1;
  }
  const flags = !redirect && !flag && !current.quoted && prefix.startsWith('-')
    ? [...(spec.flags ?? []).filter(f => !used.has(f.long)).flatMap(f => [`--${f.long}`, ...(f.short ? [`-${f.short}`] : [])]), '--help']
    : null;
  if (!flag) shape = [...(spec.required ?? []), ...(spec.optional ?? [])][argumentIndex]?.shape ?? spec.rest?.shape;
  return { command: spec.name, args: args.map(t => t.value), prefix, argumentIndex, flag, kind: redirect ? 'redirect' as const : 'argument' as const, replaceStart, flags, shape };
}

function quoteValue(value: string, close: boolean): string {
  if (/^[a-zA-Z_./][a-zA-Z0-9_./-]*$/.test(value) && !['true', 'false', 'null'].includes(value)) return value;
  return '"' + value.replace(/[\\"$]/g, c => `\\${c}`) + (close ? '"' : '');
}

export function completionEdit(line: string, start: number, prefix: string, items: CompletionItem[], literal = false) {
  const candidates = [...new Map(items.filter(i => i.value.startsWith(prefix) && !/[\x00-\x1f\x7f]/.test(i.value)).map(i => [i.value, i])).values()].sort((a, b) => a.value.localeCompare(b.value));
  if (!candidates.length) return null;
  let common = [...candidates[0]!.value];
  for (const item of candidates.slice(1)) {
    const value = [...item.value];
    const mismatch = common.findIndex((c, i) => c !== value[i]);
    if (mismatch >= 0) common = common.slice(0, mismatch);
  }
  const unique = candidates.length === 1;
  const value = common.join('');
  if (unique || value.length > prefix.length) {
    const directory = unique && candidates[0]!.directory;
    return { replacement: line.slice(0, start) + (literal ? value : quoteValue(value, unique)) + (unique && !directory ? ' ' : ''), candidates: [] };
  }
  return { replacement: undefined, candidates: candidates.map(c => c.value) };
}

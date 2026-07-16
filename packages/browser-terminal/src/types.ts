/**
 * Public types for command authors. These mirror the Rust `Signature` /
 * `Value` shapes; the serde layer accepts exactly this structure.
 */

/** A structured shell value as it appears in JavaScript. */
export type Value =
  | null
  | boolean
  | number
  | string
  | Value[]
  | { [key: string]: Value };

export type Shape = 'any' | 'str' | 'int' | 'float' | 'bool';

export interface PosArg {
  name: string;
  /** Defaults to 'any'. */
  shape?: Shape;
  desc?: string;
}

export interface FlagSpec {
  long: string;
  /** Single character, e.g. 'l'. */
  short?: string;
  /** Omit for a switch (presence → true); set for a value-taking flag. */
  shape?: Shape;
  desc?: string;
}

export interface CommandSpec {
  /** Possibly multi-word, e.g. 'dom query'. */
  name: string;
  summary?: string;
  required?: PosArg[];
  optional?: PosArg[];
  rest?: PosArg;
  flags?: FlagSpec[];
}

export interface CommandArgs {
  positionals: Value[];
  flags: Record<string, Value>;
}

export interface CommandCtx {
  /** Fires when the pipeline is aborted (Ctrl-C / dispose). Pass to fetch(). */
  signal: AbortSignal;
  /** Progressive output: print a line above the pipeline's final result. */
  emit(line: string): void;
}

export type CommandFn = (
  args: CommandArgs,
  input: Value,
  ctx: CommandCtx,
) => unknown | Promise<unknown>;

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
  /**
   * Channel 3 — progress and commentary. Goes to the terminal, never into
   * the pipe, so a downstream `| length` is unaffected by anything you log.
   */
  log(line: string): void;
  /**
   * Channel 2 — warnings and diagnostics, rendered in red. Non-fatal:
   * throw if you need to abort the pipeline.
   */
  err(line: string): void;
  /**
   * Alias for `log`, kept because it predates the channel split and every
   * existing command uses it. Prefer `log` in new code.
   */
  emit(line: string): void;
}

export type CommandFn = (
  args: CommandArgs,
  input: Value,
  ctx: CommandCtx,
) => unknown | Promise<unknown>;

/**
 * A named function usable as `@name` in any selector position (`--on`,
 * `map`, `filter`). Receives one pipeline item and returns a projection or
 * a predicate result.
 *
 * Unlike inline `'(o) => …'` source, this needs no `eval`, so it works on
 * pages whose Content-Security-Policy omits `unsafe-eval` — and it stays
 * type-checked and debuggable in devtools.
 */
export type SelectorFn = (item: Value) => unknown;

/** What `run()` resolves to: the data channel plus both diagnostic channels. */
export interface RunResult {
  /** Channel 1 — the pipeline's final structured value. */
  value: Value;
  /** Channel 3 lines, in order. */
  log: string[];
  /** Channel 2 lines, in order. */
  err: string[];
}

/**
 * What `run()` rejects with: an ordinary `Error` that also carries whatever
 * the pipeline wrote before it failed, including on Ctrl-C.
 *
 * It stays an `Error` — rather than resolving with an `error` field — so
 * `try`/`catch` and `instanceof` keep working and a failure cannot be
 * missed by an `await` that never checks.
 */
export interface RunError extends Error {
  /** Channel 3 lines written before the failure. */
  log: string[];
  /** Channel 2 lines written before the failure. */
  err: string[];
}

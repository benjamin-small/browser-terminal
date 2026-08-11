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

/**
 * A diagnostic channel. One object, two deliberately different APIs:
 *
 * - **Calling it** (`ctx.log('msg')`) passes a *message*. The shell owns the
 *   framing and sanitizing: every escape is stripped and newlines collapse,
 *   so it lands as exactly one clean single-line entry, immediately.
 * - **`.write` / `.flush` / `.mode`** pass *terminal bytes* through this
 *   channel's buffer. You own the framing; what keeps it safe is a narrow
 *   allowlist (`\r`, `\b`, `\t`, `\n`, horizontal cursor moves,
 *   erase-in-line, SGR) — everything else is still stripped.
 *
 * **Mixing the two on one channel can reorder your output.** Only `.write` /
 * `.flush` / `.mode` touch the buffer; calling the channel bypasses it
 * entirely and goes out at once. So a raw write still sitting in the buffer
 * (line mode until its delimiter, block mode until `flush()`) is overtaken
 * by a later call: `ctx.log.write('a')` then `ctx.log('b')` arrives as `b`,
 * then `a` whenever the buffer drains. If order matters, pick one API per
 * channel and stay with it — or `flush()` before switching.
 */
export interface ChannelWriter {
  /**
   * Pass a message. It is cooked — escapes stripped, newlines collapsed to
   * spaces — and emitted immediately as exactly one entry. Nothing is
   * appended to it and the buffer's delimiter is not consulted; the pane
   * supplies the line break when it displays the entry.
   */
  (line: string): void;
  /** Write without appending a delimiter: partial lines, progress bars. */
  write(s: string): void;
  /** Emit everything buffered now. Always works, in every mode. */
  flush(): void;
  /**
   * `'line'` (default) flushes on the delimiter, `'byte'` on every write,
   * `'block'` only on `flush()` or when the buffer fills.
   */
  mode(m: 'byte' | 'line' | 'block', opts?: { delimiter?: string }): void;
}

export interface CommandCtx {
  /** Fires when the pipeline is aborted (Ctrl-C / dispose). Pass to fetch(). */
  signal: AbortSignal;
  /**
   * Channel 3 — progress and commentary. Goes to the terminal, never into
   * the pipe, so a downstream `| length` is unaffected by anything you log.
   */
  log: ChannelWriter;
  /**
   * Channel 2 — warnings and diagnostics, rendered in red. Non-fatal:
   * throw if you need to abort the pipeline.
   */
  err: ChannelWriter;
  /**
   * Alias for `log`, kept because it predates the channel split and every
   * existing command uses it. Prefer `log` in new code.
   */
  emit: ChannelWriter;
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
  /**
   * Channel 3 entries, in emission order. Entries, not lines: what you may
   * rely on depends on which API produced each one, and both land here.
   *
   * - **Cooked** (`ctx.log('msg')`) — exactly one entry per call, holding no
   *   embedded newline and no escape sequence. Safe to render as a line
   *   as-is.
   * - **Raw** (`ctx.log.write(s)`) — arbitrary text. It may hold embedded
   *   newlines and any escape the writer allowlist keeps (`\r`, `\b`, `\t`,
   *   horizontal cursor moves, erase-in-line, SGR), and entry count does not
   *   track call count: line and block modes coalesce several writes into
   *   one entry. Treat these as terminal bytes, not as lines.
   */
  log: string[];
  /** Channel 2 entries, in emission order. Same shape rules as `log`. */
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
  /** Channel 3 entries written before the failure, shaped as in `RunResult`. */
  log: string[];
  /** Channel 2 entries written before the failure, shaped as in `RunResult`. */
  err: string[];
}

/**
 * Which layer a variable call targets.
 *
 * Omitted means the host layer — engine-wide, visible in every session
 * including ones created later. `{ scope: 'session', session }` targets one
 * session's own layer, which shadows the host one for that session only.
 *
 * The id comes from `bt.snapshot.sessions`, never from "whichever session
 * is active": an ambient target would land the value somewhere else if the
 * user switched sessions between your read and your write.
 */
export type VarScope = { scope?: 'host' } | { scope: 'session'; session: number };

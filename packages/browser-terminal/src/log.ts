/**
 * The library's own console output, filtered by a host-chosen level.
 *
 * Every library log line goes through here: the core's (delivered as `log`
 * events) and the wrapper's. A host command error the pane already shows is
 * `debug`; misuse such as a replaced registration is `warn`; library faults
 * are `error`.
 */

export type LogLevel = 'silent' | 'error' | 'warn' | 'info' | 'debug';

/** Where log lines go. Defaults to the global `console`. */
export type Logger = Pick<Console, 'error' | 'warn' | 'info' | 'debug'>;

const RANK: Record<LogLevel, number> = { silent: 0, error: 1, warn: 2, info: 3, debug: 4 };

export function assertLogLevel(level: unknown): asserts level is LogLevel {
  if (typeof level !== 'string' || !Object.prototype.hasOwnProperty.call(RANK, level)) {
    throw new RangeError(
      `browser-terminal: unknown log level ${JSON.stringify(level)}; expected one of ${Object.keys(RANK).join(', ')}`,
    );
  }
}

export class LogFilter {
  private current: LogLevel;

  constructor(level: LogLevel = 'warn', private readonly logger?: Logger) {
    assertLogLevel(level);
    this.current = level;
  }

  get level(): LogLevel {
    return this.current;
  }

  set level(level: LogLevel) {
    assertLogLevel(level);
    this.current = level;
  }

  log(level: Exclude<LogLevel, 'silent'>, ...args: unknown[]): void {
    if (RANK[level] > RANK[this.current]) return;
    // Resolved per call, so a console patched after create() is honoured.
    const sink = this.logger ?? globalThis.console;
    // A throwing logger must not break the terminal that called it.
    try { sink[level](...args); } catch { /* nowhere left to report it */ }
  }
}

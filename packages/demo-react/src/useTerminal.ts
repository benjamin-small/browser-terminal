/**
 * React bindings for browser-terminal.
 *
 * The whole point is the "latest ref" indirection. A shell command outlives
 * the render that registered it, so a closure captured at registration time
 * goes stale immediately:
 *
 *     useEffect(() => {
 *       bt.registerCommand({ name: 'inc' }, () => setCount(count + 1));
 *     }, []);                       // captures count === 0 forever
 *
 * Adding `count` to the deps "fixes" it by re-registering on every render,
 * which is wasteful and — because the registry treats re-registration as
 * hot-reload — logs a warning each time.
 *
 * Instead: register ONE stable wrapper that reads the freshest closure out of
 * a ref. Registration happens once per command name; the behavior is always
 * current. This is the same shape as React's own useEffectEvent.
 */
import { useEffect, useLayoutEffect, useRef } from 'react';
import type { BrowserTerminal, CommandFn, CommandSpec, SelectorFn } from 'browser-terminal';

/**
 * Register a shell command for the lifetime of the component.
 *
 * `spec` is captured on first registration and re-read only when
 * `spec.name` changes — the description of a command is static in practice,
 * while its *implementation* closes over state that changes every render.
 */
// #region hook
export function useCommand(
  bt: BrowserTerminal | null,
  spec: CommandSpec,
  fn: CommandFn,
): void {
  const fnRef = useRef(fn);
  const specRef = useRef(spec);
  // Layout effect so the ref is current before anything can invoke it.
  useLayoutEffect(() => {
    fnRef.current = fn;
    specRef.current = spec;
  });

  useEffect(() => {
    if (!bt) return;
    const name = specRef.current.name;
    bt.registerCommand(specRef.current, (...args) =>
      (fnRef.current as (...a: unknown[]) => unknown)(...args),
    );
    return () => bt.unregisterCommand(name);
  }, [bt, spec.name]);
}
// #endregion

/** Same pattern for `@name` selector functions (`map @slug`). */
export function useSelectorFn(
  bt: BrowserTerminal | null,
  name: string,
  fn: SelectorFn,
): void {
  const fnRef = useRef(fn);
  useLayoutEffect(() => {
    fnRef.current = fn;
  });

  useEffect(() => {
    if (!bt) return;
    bt.registerFn(name, (item) => fnRef.current(item));
    return () => bt.unregisterFn(name);
  }, [bt, name]);
}

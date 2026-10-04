import { test } from 'node:test';
import assert from 'node:assert/strict';
import { LogFilter } from '../dist/log.js';

function recorder() {
  const calls = [];
  const sink = Object.fromEntries(
    ['error', 'warn', 'info', 'debug'].map(level => [level, (...args) => calls.push([level, ...args])]),
  );
  return { calls, sink };
}

test('the default level emits warn and error only', () => {
  const { calls, sink } = recorder();
  const logs = new LogFilter(undefined, sink);
  for (const level of ['error', 'warn', 'info', 'debug']) logs.log(level, level);
  assert.equal(logs.level, 'warn');
  assert.deepEqual(calls, [['error', 'error'], ['warn', 'warn']]);
});

test('each level admits itself and everything more severe', () => {
  const expected = { silent: [], error: ['error'], warn: ['error', 'warn'], info: ['error', 'warn', 'info'], debug: ['error', 'warn', 'info', 'debug'] };
  for (const [level, admitted] of Object.entries(expected)) {
    const { calls, sink } = recorder();
    const logs = new LogFilter(level, sink);
    for (const l of ['error', 'warn', 'info', 'debug']) logs.log(l, 'x');
    assert.deepEqual(calls.map(([l]) => l), admitted, level);
  }
});

test('the level changes at runtime and rejects unknown names', () => {
  const { calls, sink } = recorder();
  const logs = new LogFilter('silent', sink);
  logs.log('error', 'hidden');
  logs.level = 'debug';
  logs.log('debug', 'shown', 42);
  assert.deepEqual(calls, [['debug', 'shown', 42]]);
  assert.throws(() => { logs.level = 'verbose'; }, RangeError);
  assert.throws(() => new LogFilter('loud'), RangeError);
  assert.equal(logs.level, 'debug', 'a rejected level leaves the old one');
});

test('without a logger the global console is used, resolved per call', () => {
  const original = console.warn;
  const seen = [];
  try {
    const logs = new LogFilter('warn');
    console.warn = (...args) => seen.push(args);
    logs.log('warn', 'patched later');
  } finally {
    console.warn = original;
  }
  assert.deepEqual(seen, [['patched later']]);
});

test('a throwing logger does not throw into the caller', () => {
  const logs = new LogFilter('debug', { error() { throw new Error('sink down'); }, warn() {}, info() {}, debug() {} });
  assert.doesNotThrow(() => logs.log('error', 'x'));
});

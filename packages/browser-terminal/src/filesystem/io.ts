import type { FileHandle, WriteTransaction } from './types.js';
import { checkRange, integer } from './paths.js';

/** One adapter-wide write gate also covers aliases through overlapping mounts.
 * Conservative serialization avoids assuming virtual path equality is file identity.
 */
export class WriteGate {
  private tail: Promise<void> = Promise.resolve();
  async acquire(signal: AbortSignal): Promise<() => void> {
    signal.throwIfAborted();
    const prior = this.tail;
    let release!: () => void;
    const done = new Promise<void>(resolve => { release = resolve; });
    this.tail = prior.then(() => done);
    // Release the queue slot even when its waiter is cancelled before acquisition.
    let abort!: () => void;
    const cancelled = new Promise<never>((_, reject) => {
      abort = () => { release(); reject(signal.reason); };
      signal.addEventListener('abort', abort, { once: true });
    });
    try { await Promise.race([prior, cancelled]); signal.throwIfAborted(); }
    catch (error) { release(); throw error; }
    finally { signal.removeEventListener('abort', abort); }
    return release;
  }
}

export async function openTransaction(
  handle: FileHandle, signal: AbortSignal, check: () => void, release: () => void,
  maxChunk: number, checkCommit: () => Promise<void>,
): Promise<WriteTransaction> {
  let stream: FileSystemWritableFileStream | undefined;
  try {
    check();
    if (typeof handle.createWritable !== 'function') throw new Error('This browser does not support writable file streams');
    const file = await handle.getFile();
    check();
    stream = await handle.createWritable({ keepExistingData: true });
    check();
    const writable = stream;
    let state: 'open' | 'committing' | 'closed' = 'open';
    let pending = Promise.resolve();
    let failure: unknown;
    const finish = () => { state = 'closed'; signal.removeEventListener('abort', onAbort); release(); };
    const abort = async () => {
      if (state !== 'open') return;
      state = 'closed';
      try { await pending; await writable.abort(); } finally { finish(); }
    };
    const onAbort = () => { void abort().catch(() => {}); };
    signal.addEventListener('abort', onAbort, { once: true });
    const enqueue = (operation: () => Promise<void>) => {
      if (state !== 'open') return Promise.reject(new Error('Write transaction is closed'));
      const result = pending.then(async () => { check(); if (failure) throw failure; await operation(); check(); });
      pending = result.catch(error => { failure = error; });
      return result;
    };
    return {
      size: file.size,
      writeAt(offset, bytes) {
        checkRange(offset, bytes.length);
        if (bytes.length > maxChunk) throw new Error(`Write chunk exceeds ${maxChunk} bytes`);
        const copy = new Uint8Array(bytes);
        return enqueue(() => writable.write({ type: 'write', position: offset, data: copy }));
      },
      truncate(size) { integer(size, 'size'); return enqueue(() => writable.truncate(size)); },
      async commit() {
        if (state !== 'open') throw new Error('Write transaction is closed');
        state = 'committing';
        try {
          await pending;
          if (failure) throw failure;
          await checkCommit();
          check();
          await writable.close();
        } catch (error) {
          try { await writable.abort(); } catch { /* Preserve the original failure. */ }
          throw error;
        } finally { finish(); }
      },
      abort,
    };
  } catch (error) {
    try { await stream?.abort(); } catch { /* Preserve the open error. */ }
    release();
    throw error;
  }
}

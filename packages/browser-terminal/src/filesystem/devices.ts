import type { BrowserFilesystem } from './index.js';
import type { FsContext } from './types.js';
import { checkRange, integer, normalizePath } from './paths.js';

/** Experimental fixed-capacity file-backed blocks. Each write commits separately. */
export async function openBlockDevice(fs: BrowserFilesystem, path: string, ctx: FsContext, blockSize = 512) {
  integer(blockSize, 'blockSize');
  if (!blockSize) throw new Error('blockSize must be positive');
  const absolute = normalizePath(path, fs.pwd(ctx.session));
  const document = await fs.document(absolute, ctx);
  const context = { ...ctx, signal: AbortSignal.any([document.signal, ...(ctx.signal ? [ctx.signal] : [])]) };
  const reader = await fs.openReader(absolute, context);
  const capacity = reader.size;
  if (capacity === null || capacity % blockSize) throw new Error('Block device needs a file aligned to blockSize');
  const range = (block: number, count: number) => {
    checkRange(block, count);
    const offset = block * blockSize;
    const length = count * blockSize;
    checkRange(offset, length);
    if (offset + length > capacity) throw new Error('Block access exceeds capacity');
    return { offset, length };
  };
  return {
    blockSize, capacity,
    capabilities: Object.freeze({ read: true, write: document.writable, seek: true, truncate: false, stream: false }),
    async read(block: number, count = 1) {
      const { offset, length } = range(block, count);
      const current = await fs.openReader(absolute, context);
      if (current.size !== capacity) throw new Error('Block device size changed');
      return current.readAt(offset, length);
    },
    async write(block: number, bytes: Uint8Array) {
      if (bytes.length % blockSize) throw new Error('Block writes must be aligned');
      const { offset } = range(block, bytes.length / blockSize);
      const tx = await fs.beginWrite(absolute, context);
      try {
        if (tx.size !== capacity) throw new Error('Block device size changed');
        await tx.writeAt(offset, bytes); await tx.commit();
      } catch (error) { await tx.abort().catch(() => {}); throw error; }
    },
  };
}

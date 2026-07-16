/**
 * Per-pane xterm plumbing: a write queue that chains through `term.write`
 * callbacks so a giant rendered table can never overrun xterm's internal
 * buffer or freeze the frame.
 */
import type { Terminal } from '@xterm/xterm';

/** Split very large single writes into slices this big. */
const CHUNK_SIZE = 64 * 1024;

export class PaneWriter {
  private queue: string[] = [];
  private writing = false;
  private disposed = false;

  constructor(private readonly term: Terminal) {}

  write(data: string): void {
    if (this.disposed) return;
    for (let i = 0; i < data.length; i += CHUNK_SIZE) {
      this.queue.push(data.slice(i, i + CHUNK_SIZE));
    }
    this.pump();
  }

  dispose(): void {
    this.disposed = true;
    this.queue.length = 0;
  }

  private pump(): void {
    if (this.writing || this.disposed) return;
    const chunk = this.queue.shift();
    if (chunk === undefined) return;
    this.writing = true;
    this.term.write(chunk, () => {
      this.writing = false;
      this.pump();
    });
  }
}

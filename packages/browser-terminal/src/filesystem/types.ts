import type { CommandFn, CommandSpec, RedirectContext } from '../types.js';

export interface FileHandle {
  readonly kind: 'file';
  readonly name: string;
  getFile(): Promise<File>;
  createWritable(options?: { keepExistingData?: boolean }): Promise<FileSystemWritableFileStream>;
  isSameEntry(other: FileSystemHandle): Promise<boolean>;
}
export interface DirectoryHandle {
  readonly kind: 'directory';
  readonly name: string;
  getDirectoryHandle(name: string, options?: { create?: boolean }): Promise<DirectoryHandle>;
  getFileHandle(name: string, options?: { create?: boolean }): Promise<FileHandle>;
  values(): AsyncIterableIterator<DirectoryHandle | FileHandle>;
}
export interface FsContext {
  session: number;
  signal?: AbortSignal;
}
export interface Entry {
  [key: string]: string | number | null;
  name: string;
  kind: string;
  path: string;
}
export interface Capabilities {
  read: boolean;
  write: boolean;
  seek: boolean;
  truncate: boolean;
  stream: boolean;
}
export interface ByteReader {
  readonly size: number | null;
  readonly capabilities: Readonly<Capabilities>;
  readAt(offset: number, length: number): Promise<Uint8Array>;
  chunks(options?: { offset?: number; length?: number; chunkSize?: number }): AsyncIterable<Uint8Array>;
}
export interface WriteTransaction {
  readonly size: number;
  writeAt(offset: number, bytes: Uint8Array): Promise<void>;
  truncate(size: number): Promise<void>;
  commit(): Promise<void>;
  abort(): Promise<void>;
}
export interface EditorDocument {
  readonly path: string;
  readonly writable: boolean;
  readonly signal: AbortSignal;
  read(): Promise<Uint8Array>;
  save(bytes: Uint8Array, original: Uint8Array): Promise<void>;
  requestWritePermission(): Promise<boolean>;
}
export interface FilesystemOptions {
  maxReadBytes?: number;
  maxEditorBytes?: number;
  chunkSize?: number;
  devices?: boolean;
  editor?: (document: EditorDocument) => void | Promise<void>;
}
/** Minimal host contract also supports deterministic tests without loading WASM. */
export interface FilesystemHost {
  registerOwnedCommand(spec: CommandSpec, fn: CommandFn): () => void;
  onLifecycle(listener: (event: { type: 'dispose' } | { type: 'sessionClosed'; session: number }) => void): () => void;
}
export type RedirectFsContext = Pick<RedirectContext, 'session' | 'signal'>;

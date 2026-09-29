/** Virtual paths never expose or traverse a selected directory's real parent. */
export function normalizePath(path: string, cwd = '/'): string {
  if (path.includes('\0')) throw new Error('Paths cannot contain NUL');
  const parts: string[] = [];
  for (const part of (path.startsWith('/') ? path : `${cwd}/${path}`).split('/')) {
    if (!part || part === '.') continue;
    if (part === '..') parts.pop();
    else parts.push(part);
  }
  return `/${parts.join('/')}`;
}
export function integer(value: number, name: string): number {
  if (!Number.isSafeInteger(value) || value < 0) throw new Error(`${name} must be a nonnegative safe integer`);
  return value;
}
export function checkRange(offset: number, length: number): void {
  integer(offset, 'offset');
  integer(length, 'length');
  integer(offset + length, 'range end');
}
export function sameBytes(a: Uint8Array, b: Uint8Array): boolean {
  return a.length === b.length && a.every((byte, index) => byte === b[index]);
}
export function decodeText(bytes: Uint8Array): string {
  return new TextDecoder('utf-8', { fatal: true }).decode(bytes);
}

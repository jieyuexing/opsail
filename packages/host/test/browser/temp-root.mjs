/** Short socket fixture names, still owned by the caller's task TMPDIR. */
import { tmpdir as systemTmpdir } from 'node:os';
import { resolve } from 'node:path';
export function tmpdir() {
  const root = systemTmpdir();
  return resolve(root) === process.cwd() ? '.' : root;
}

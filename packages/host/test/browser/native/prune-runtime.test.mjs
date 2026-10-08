import assert from 'node:assert/strict';
import test from 'node:test';
import {mkdtemp, rm, readFile, writeFile, mkdir, chmod, symlink, lstat, readdir} from 'node:fs/promises';
import {tmpdir} from '../temp-root.mjs';
import path from 'node:path';
import {createServer} from 'node:net';
import {execFile} from 'node:child_process';
import {promisify} from 'node:util';
import {atomicPrivateJson, runtimeSocketPath} from '../../../src/browser/native/common.mjs';
import {pruneRuntime} from '../../../src/browser/prune-runtime.mjs';

async function fixture(t) {
  const root = await mkdtemp(path.join(tmpdir(), 'p-'));
  t.after(() => rm(root, {recursive: true, force: true}));
  const config = {schemaVersion: 1, dataRoot: root, extensionId: 'a'.repeat(32), buildId: 'b'.repeat(64), nodePath: process.execPath, providers: {feishu: {profileId: 'aa'}, teams: {profileId: 'bb', pending: true}}};
  await atomicPrivateJson(path.join(root, 'config.json'), config);
  const record = async id => atomicPrivateJson(path.join(root, `runtime/${id}.json`), {schemaVersion: 1, profileId: id, socket: path.basename(runtimeSocketPath(root, id))});
  return {root, config, record};
}
async function live(t, root, id) {
  const server = createServer(socket => socket.destroy());
  await new Promise((resolve, reject) => server.once('error', reject).listen(runtimeSocketPath(root, id), resolve));
  await chmod(runtimeSocketPath(root, id), 0o600);
  t.after(() => new Promise(resolve => server.close(resolve)));
}

test('prune removes only unbound offline records; bound, pending and live profiles plus all other data survive', async t => {
  const f = await fixture(t);
  for (const id of ['aa', 'bb', 'cc', 'dd']) await f.record(id);
  await live(t, f.root, 'dd');
  const protectedFiles = ['config.json', 'bindings.json', 'runtime/releases/build/extension.js', 'packages/chat.json', 'runtime/disabled.json'];
  for (const file of protectedFiles.slice(1)) { await mkdir(path.dirname(path.join(f.root, file)), {recursive: true}); await writeFile(path.join(f.root, file), 'UNCHANGED'); }
  const before = await Promise.all(protectedFiles.map(file => readFile(path.join(f.root, file), 'utf8')));
  const result = await pruneRuntime(f.root);
  assert.deepEqual(result.removed, [{profileId: 'cc', record: 'runtime/cc.json'}]);
  assert.deepEqual(result.kept, [{profileId: 'aa', reason: 'bound'}, {profileId: 'bb', reason: 'bound'}, {profileId: 'dd', reason: 'live'}]);
  assert.equal(result.removedCount, 1);
  assert.deepEqual(await Promise.all(protectedFiles.map(file => readFile(path.join(f.root, file), 'utf8'))), before);
  assert.equal((await lstat(runtimeSocketPath(f.root, 'dd'))).isSocket(), true);
  assert.equal((await pruneRuntime(f.root)).removedCount, 0);
});

test('symlink/unsafe/corrupt records and uncertain sockets are preserved without following metadata paths', async t => {
  const f = await fixture(t);
  await f.record('cc'); await chmod(path.join(f.root, 'runtime/cc.json'), 0o644);
  await f.record('dd'); await writeFile(runtimeSocketPath(f.root, 'dd'), 'occupied');
  await f.record('ee'); await symlink(path.join(f.root, 'config.json'), runtimeSocketPath(f.root, 'ee'));
  await symlink(path.join(f.root, 'config.json'), path.join(f.root, 'runtime/ff.json'));
  await atomicPrivateJson(path.join(f.root, 'runtime/11.json'), {schemaVersion: 1, profileId: '22', socket: '../config.json'});
  await writeFile(path.join(f.root, 'runtime/22.json'), 'invalid json', {mode: 0o600});
  await f.record('33'); await live(t, f.root, '33'); await chmod(runtimeSocketPath(f.root, '33'), 0o666);
  const result = await pruneRuntime(f.root);
  assert.equal(result.removedCount, 0); assert.equal(result.kept.length, 7);
  assert.equal((await lstat(path.join(f.root, 'runtime/ff.json'))).isSymbolicLink(), true);
  assert.equal((await lstat(runtimeSocketPath(f.root, 'ee'))).isSymbolicLink(), true);
});

test('cleanup refuses a held config/startup lock or uncertain config without removing a record', async t => {
  const f = await fixture(t); await f.record('cc');
  await mkdir(path.join(f.root, 'runtime/config.lock'));
  await assert.rejects(pruneRuntime(f.root), {code: 'binding-busy'});
  await rm(path.join(f.root, 'runtime/config.lock'), {recursive: true});
  await atomicPrivateJson(path.join(f.root, 'config.json'), {...f.config, providers: {feishu: {pending: true}}});
  await assert.rejects(pruneRuntime(f.root), {code: 'invalid-config'});
  await writeFile(path.join(f.root, 'config.json'), 'broken');
  await assert.rejects(pruneRuntime(f.root), {code: 'invalid-config'});
  await rm(path.join(f.root, 'config.json'));
  await assert.rejects(pruneRuntime(f.root), {code: 'invalid-config'});
  assert.equal((await lstat(path.join(f.root, 'runtime/cc.json'))).isFile(), true);
});

test('cleanup rejects symlinked runtime roots and oversized inventories before deletion', async t => {
  const f = await fixture(t);
  const foreign = path.join(f.root, 'other'); await mkdir(foreign);
  await symlink(foreign, path.join(f.root, 'runtime'));
  await assert.rejects(pruneRuntime(f.root), {code: 'unsafe-path'});
  await rm(path.join(f.root, 'runtime'));
  for (let i = 256; i < 385; i++) await f.record(i.toString(16));
  await assert.rejects(pruneRuntime(f.root), {code: 'runtime-record-limit'});
  assert.equal((await readdir(path.join(f.root, 'runtime'))).length, 129);
});

test('prune-runtime CLI prints exact removals and is idempotent on an isolated data root', async t => {
  const f = await fixture(t); await f.record('cc');
  const run = () => promisify(execFile)(process.execPath, [new URL('../../../src/browser/opsail-chrome.mjs', import.meta.url).pathname, 'prune-runtime', '--data-root', f.root]);
  const first = JSON.parse((await run()).stdout);
  assert.equal(first.ok, true); assert.equal(first.removedCount, 1);
  assert.deepEqual(first.removed, [{profileId: 'cc', record: 'runtime/cc.json'}]);
  assert.equal(JSON.parse((await run()).stdout).removedCount, 0);
});

test('refused compact socket permits metadata pruning but the socket itself is never deleted', async t => {
  const f = await fixture(t), id = '0'.repeat(32);
  await f.record(id);
  const socketPath = runtimeSocketPath(f.root, id);
  await promisify(execFile)('python3', ['-c', 'import socket,sys,os; s=socket.socket(socket.AF_UNIX); s.bind(sys.argv[1]); os.chmod(sys.argv[1],0o600); s.close()', socketPath]);
  const result = await pruneRuntime(f.root);
  assert.deepEqual(result.removed, [{profileId: id, record: `runtime/${id}.json`}]);
  assert.equal((await lstat(socketPath)).isSocket(), true);
});

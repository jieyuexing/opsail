import { tmpdir } from '../temp-root.mjs';
import assert from 'node:assert/strict';
import { createConnection } from 'node:net';
import { once } from 'node:events';
import { mkdtemp, mkdir, readFile, stat, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { spawn } from 'node:child_process';
import test from 'node:test';
import { PROTOCOL_VERSION, atomicPrivateJson, defaultBindingFile, defaultDataRoot, encodeLine } from '../../../src/browser/native/common.mjs';
import {request} from '../../../src/browser/client.mjs';
import { doctor, install } from '../../../src/browser/install.mjs';
import { resolveOpsailLayout } from '../../../src/layout.js';

const extensionId = 'abcdefghijklmnopabcdefghijklmnop';
function frame(value) { const json = Buffer.from(JSON.stringify(value)); const size = Buffer.alloc(4); size.writeUInt32LE(json.length); return Buffer.concat([size, json]); }
test('default chat storage and bindings resolve from the shared Opsail layout', () => {
  const layout = resolveOpsailLayout();
  assert.equal(defaultDataRoot, layout.retainedChatDataDir);
  assert.equal(defaultBindingFile, layout.chatBindingFile);
  assert.equal(defaultDataRoot, path.resolve(process.env.OPSAIL_CHAT_DATA_ROOT || layout.retainedChatDataDir));
  assert.equal(defaultBindingFile, path.resolve(process.env.OPSAIL_CHAT_BINDING_FILE || path.join(defaultDataRoot, 'bindings.json')));
});

// Every spawned host is stopped in a finally block: a failed assertion must never
// leave a child holding the test runner's stdio open, which hangs `node --test`.
function spawnHost(root, origin) {
  const child = spawn(process.execPath, [path.join(path.dirname(new URL(import.meta.url).pathname), '../../../src/browser/native/native-host.mjs'), origin], { env: { ...process.env, OPSAIL_CHROME_DATA_ROOT: root } });
  return { child, closed: once(child, 'close') };
}
async function stopHost({ child, closed }) {
  if (child.exitCode === null && child.signalCode === null) child.kill('SIGTERM');
  const force = setTimeout(() => { try { child.kill('SIGKILL'); } catch { /* already gone */ } }, 2000);
  try { await closed; } finally { clearTimeout(force); }
}
const HOST_TEST = { timeout: 15000 };

function nativeReader(stream) {
  let buffer = Buffer.alloc(0); const messages = []; let wake;
  stream.on('data', (chunk) => { buffer = Buffer.concat([buffer, chunk]); while (buffer.length >= 4 && buffer.length >= buffer.readUInt32LE(0) + 4) { const n = buffer.readUInt32LE(0); messages.push(JSON.parse(buffer.subarray(4, n + 4))); buffer = buffer.subarray(n + 4); if (wake) { wake(); wake = null; } } });
  return async () => { while (!messages.length) await new Promise((resolve) => { wake = resolve; }); return messages.shift(); };
}

test('native host rejects a different extension origin before serving IPC', HOST_TEST, async () => {
  const root = await mkdtemp(path.join(tmpdir(), 'n-'));
  await atomicPrivateJson(path.join(root, 'config.json'), { schemaVersion: 1, dataRoot: root, extensionId, buildId: 'build', nodePath: process.execPath, providers: {} });
  const host = spawnHost(root, 'chrome-extension://pppppppppppppppppppppppppppppppp/');
  try {
    const read = nativeReader(host.child.stdout); const message = await read();
    assert.equal(message.error.code, 'origin-denied');
  } finally { await stopHost(host); }
});

test('native host closes after a rejected hello version', HOST_TEST, async () => {
  const root = await mkdtemp(path.join(tmpdir(), 'n-'));
  await atomicPrivateJson(path.join(root, 'config.json'), { schemaVersion: 1, dataRoot: root, extensionId, buildId: 'build', nodePath: process.execPath, providers: {} });
  const host = spawnHost(root, `chrome-extension://${extensionId}/`);
  try {
    const read = nativeReader(host.child.stdout);
    host.child.stdin.write(frame({ type: 'hello', protocolVersion: 1, extensionVersion: '9.9.9', buildId: 'build', profileId: 'a1b2' }));
    assert.equal((await read()).error.code, 'handshake-rejected');
    // The host must exit on its own after rejecting the handshake.
    await host.closed;
  } finally { await stopHost(host); }
});

test('native host forwards only allowed request and injects local binding', HOST_TEST, async () => {
  const root = await mkdtemp(path.join(tmpdir(), 'n-')); const profileId = 'a1b2c3';
  await atomicPrivateJson(path.join(root, 'config.json'), { schemaVersion: 1, dataRoot: root, extensionId, buildId: 'build', nodePath: process.execPath, providers: { feishu: { profileId, targetUrl: 'https://x.feishu.cn/next/', allowedOrigins: ['https://x.feishu.cn'], accountHash: 'a'.repeat(64), tenantHash: 'b'.repeat(64) } } });
  const host = spawnHost(root, `chrome-extension://${extensionId}/`); const { child } = host;
  try {
    const read = nativeReader(child.stdout);
    child.stdin.write(frame({ type: 'hello', protocolVersion: 1, extensionVersion: '0.1.0', buildId: 'build', profileId }));
    const helloReply = await read();
    assert.equal(helloReply.type, 'hello-ok', JSON.stringify(helloReply));
    const replyPromise = new Promise((resolve, reject) => {
      const socket = createConnection(path.join(root, 'runtime', `${profileId}.sock`)); let buffer = '';
      socket.on('connect', () => socket.write(encodeLine({ protocolVersion: 1, requestId: 'r1', operation: 'read', provider: 'feishu', args: {} })));
      socket.on('data', (chunk) => { buffer += chunk; if (buffer.includes('\n')) { socket.end(); resolve(JSON.parse(buffer)); } }); socket.on('error', reject);
    });
    // The host must first emit the constrained request; answer it before accepting the client response.
    const forwarded = await read();
    assert.equal(forwarded.operation, 'read'); assert.equal(forwarded.binding.accountHash, 'a'.repeat(64));
    child.stdin.write(frame({ type: 'response', requestId: 'r1', ok: true, data: { messages: [] } }));
    const response = await replyPromise;
    assert.equal(response.ok, true); assert.deepEqual(response.data, { messages: [] });
    assert.deepEqual(forwarded.binding.identityBuilds, ['index.745e4057.js']);
    // Native host re-reads only its private allow-list; a caller cannot inject it.
    const installed = JSON.parse(await readFile(path.join(root, 'config.json'), 'utf8'));
    installed.identityBuilds = {feishu: [{name: 'index.abcdef01.js', qualification: 'qualified-by-operator'}]};
    await atomicPrivateJson(path.join(root, 'config.json'), installed);
    const failure = request('status', 'feishu', {}, {dataRoot: root, profileId});
    const failureAssertion = assert.rejects(failure, error => {
      assert.equal(error.code, 'unqualified-build');
      assert.deepEqual(error.diagnostics.bundleNames, ['index.11111111.js']);
      assert.doesNotMatch(JSON.stringify(error), /PRIVATE/);
      return true;
    });
    const next = await read(); assert.deepEqual(next.binding.identityBuilds, ['index.abcdef01.js']);
    child.stdin.write(frame({type: 'response', requestId: next.requestId, ok: false, error: {code: 'unqualified-build', message: 'PRIVATE', diagnostics: {bundleNames: ['index.11111111.js', 'https://PRIVATE/'], accountId: 'PRIVATE', buildQualified: false}}}));
    await failureAssertion;
    const rejected = await new Promise((resolve, reject) => {
      const socket = createConnection(path.join(root, 'runtime', `${profileId}.sock`)); let buffer = '';
      socket.on('connect', () => socket.write(encodeLine({ protocolVersion: 1, requestId: 'keep-id', operation: 'arbitrary', args: {} })));
      socket.on('data', (chunk) => { buffer += chunk; if (buffer.includes('\n')) { socket.end(); resolve(JSON.parse(buffer)); } }); socket.on('error', reject);
    });
    assert.equal(rejected.requestId, 'keep-id'); assert.equal(rejected.error.code, 'invalid-request');
    await atomicPrivateJson(path.join(root, 'config.json'), { schemaVersion: 1, dataRoot: root, extensionId, buildId: 'replacement-build', nodePath: process.execPath, providers: { feishu: { profileId, targetUrl: 'https://x.feishu.cn/next/', allowedOrigins: ['https://x.feishu.cn'] } } });
    const stale = await new Promise((resolve, reject) => {
      const socket = createConnection(path.join(root, 'runtime', `${profileId}.sock`)); let buffer = '';
      socket.on('connect', () => socket.write(encodeLine({ protocolVersion: 1, requestId: 'build-change', operation: 'ping', args: {} })));
      socket.on('data', (chunk) => { buffer += chunk; if (buffer.includes('\n')) { socket.end(); resolve(JSON.parse(buffer)); } }); socket.on('error', reject);
    });
    assert.equal((await read()).type, 'build-changed');
    assert.equal(stale.requestId, 'build-change'); assert.equal(stale.error.code, 'build-changed');
    await atomicPrivateJson(path.join(root, 'config.json'), { schemaVersion: 1, dataRoot: root, extensionId, buildId: 'build', nodePath: process.execPath, providers: { feishu: { profileId, targetUrl: 'https://x.feishu.cn/next/', allowedOrigins: ['https://x.feishu.cn'] } } });
    await atomicPrivateJson(path.join(root, 'runtime', 'disabled.json'), { schemaVersion: 1, disabledAt: '2026-09-16T00:00:00.000Z', reason: 'uninstalled' });
    const disabled = await new Promise((resolve, reject) => {
      const socket = createConnection(path.join(root, 'runtime', `${profileId}.sock`)); let buffer = '';
      socket.on('connect', () => socket.write(encodeLine({ protocolVersion: 1, requestId: 'disabled', operation: 'ping', args: {} })));
      socket.on('data', (chunk) => { buffer += chunk; if (buffer.includes('\n')) { socket.end(); resolve(JSON.parse(buffer)); } }); socket.on('error', reject);
    });
    assert.equal(disabled.error.code, 'bridge-disabled');
  } finally { await stopHost(host); }
});

test('native host rejects a second active host for the same profile', HOST_TEST, async () => {
  const root = await mkdtemp(path.join(tmpdir(), 'n-')); const profileId = 'deadbeef';
  const config = { schemaVersion: 1, dataRoot: root, extensionId, buildId: 'build', nodePath: process.execPath, providers: {} };
  await atomicPrivateJson(path.join(root, 'config.json'), config);
  const origin = `chrome-extension://${extensionId}/`;
  const one = spawnHost(root, origin); let two = null;
  try {
    const oneRead = nativeReader(one.child.stdout);
    one.child.stdin.write(frame({ type: 'hello', protocolVersion: 1, extensionVersion: '0.1.0', buildId: 'build', profileId }));
    const helloReply = await oneRead();
    assert.equal(helloReply.type, 'hello-ok', JSON.stringify(helloReply));
    two = spawnHost(root, origin); const twoRead = nativeReader(two.child.stdout);
    two.child.stdin.write(frame({ type: 'hello', protocolVersion: 1, extensionVersion: '0.1.0', buildId: 'build', profileId }));
    assert.equal((await twoRead()).error.code, 'profile-active');
  } finally { await Promise.all([stopHost(one), two ? stopHost(two) : null]); }
});

test('install stages an unpacked extension and does not need Chrome preferences', async () => {
  const root = await mkdtemp(path.join(tmpdir(), 'opsail-install-')); const source = path.join(root, 'source'); const home = path.join(root, 'home');
  await mkdir(source, { recursive: true });
  await writeFile(path.join(source, 'manifest.json'), JSON.stringify({ manifest_version: 3, name: 'test', version: '0.1.0', key: Buffer.from('fixed-test-public-key').toString('base64') }));
  await mkdir(path.join(root, 'data', 'runtime'), { recursive: true }); await mkdir(path.join(root, 'data', 'packages', 'retained'), { recursive: true });
  await writeFile(path.join(root, 'data', 'packages', 'retained', 'message.json'), '{}');
  await atomicPrivateJson(path.join(root, 'data', 'runtime', 'disabled.json'), { schemaVersion: 1, reason: 'uninstalled' });
  const result = await install({ dataRoot: path.join(root, 'data'), extensionSource: source, home });
  assert.match(result.extensionId, /^[a-p]{32}$/); assert.match(result.unpackedPath, /runtime\/current\/extension$/);
  assert.equal(JSON.parse(await readFile(path.join(result.unpackedPath, 'build.json'), 'utf8')).protocolVersion, PROTOCOL_VERSION);
  assert.equal((await stat(result.unpackedPath)).mode & 0o777, 0o700);
  assert.equal((await stat(path.join(result.unpackedPath, 'manifest.json'))).mode & 0o777, 0o600);
  assert.equal((await doctor(path.join(root, 'data'))).configured, true);
  await assert.rejects(readFile(path.join(root, 'data', 'runtime', 'disabled.json'))); assert.equal(await readFile(path.join(root, 'data', 'packages', 'retained', 'message.json'), 'utf8'), '{}');
});


test('native hello build mismatch requests reload and never starts IPC', HOST_TEST, async () => {
  const root = await mkdtemp(path.join(tmpdir(), 'n-'));
  await atomicPrivateJson(path.join(root, 'config.json'), {schemaVersion: 1, dataRoot: root, extensionId, buildId: 'b'.repeat(64), nodePath: process.execPath, providers: {}});
  const host = spawnHost(root, `chrome-extension://${extensionId}/`);
  try {
    const read = nativeReader(host.child.stdout);
    host.child.stdin.write(frame({type: 'hello', protocolVersion: 1, extensionVersion: '0.1.0', buildId: 'a'.repeat(64), profileId: 'ab'}));
    assert.deepEqual(await read(), {type: 'build-changed', buildId: 'b'.repeat(64)});
    assert.equal((await read()).error.code, 'build-changed');
    await host.closed;
    await assert.rejects(stat(path.join(root, 'runtime/ab.sock')));
  } finally { await stopHost(host); }
});


test('native binding probe uses only ephemeral validated coordinates and cleans its runtime record', HOST_TEST, async () => {
  const root = await mkdtemp(path.join(tmpdir(), 'n-'));
  const file = path.join(root, 'config.json');
  await atomicPrivateJson(file, {schemaVersion: 1, dataRoot: root, extensionId, buildId: 'fixture', nodePath: process.execPath, providers: {}});
  const before = await readFile(file, 'utf8');
  const host = spawnHost(root, `chrome-extension://${extensionId}/`);
  try {
    const read = nativeReader(host.child.stdout);
    host.child.stdin.write(frame({type: 'hello', protocolVersion: 1, extensionVersion: '0.1.0', buildId: 'fixture', profileId: 'ab'}));
    assert.equal((await read()).type, 'hello-ok');
    const binding = {targetUrl: 'https://x.feishu.cn/next/', allowedOrigins: ['https://x.feishu.cn'], accountHash: 'a'.repeat(64), tenantHash: 'b'.repeat(64)};
    const reply = request('verifyBinding', 'feishu', {}, {dataRoot: root, profileId: 'ab', binding});
    const forwarded = await read();
    assert.equal(forwarded.operation, 'verifyBinding'); assert.deepEqual(forwarded.binding, {...binding, identityBuilds: ['index.745e4057.js']});
    host.child.stdin.write(frame({type: 'response', requestId: forwarded.requestId, ok: true, data: {ready: true}}));
    assert.equal((await reply).ready, true);
    assert.equal(await readFile(file, 'utf8'), before);
  } finally { await stopHost(host); }
  await assert.rejects(stat(path.join(root, 'runtime/ab.json')));
});

test('native startup respects runtime cleanup lock and never publishes metadata before acquiring it', HOST_TEST, async () => {
  const root = await mkdtemp(path.join(tmpdir(), 'n-'));
  await atomicPrivateJson(path.join(root, 'config.json'), {schemaVersion: 1, dataRoot: root, extensionId, buildId: 'build', nodePath: process.execPath, providers: {}});
  await mkdir(path.join(root, 'runtime/config.lock'), {recursive: true});
  const host = spawnHost(root, `chrome-extension://${extensionId}/`);
  try {
    const read = nativeReader(host.child.stdout);
    host.child.stdin.write(frame({type: 'hello', protocolVersion: 1, extensionVersion: '0.1.0', buildId: 'build', profileId: 'abc'}));
    assert.equal((await read()).error.code, 'binding-busy');
    await host.closed;
    await assert.rejects(stat(path.join(root, 'runtime/abc.json')), {code: 'ENOENT'});
    assert.equal((await stat(path.join(root, 'runtime/config.lock'))).isDirectory(), true);
  } finally { await stopHost(host); }
});

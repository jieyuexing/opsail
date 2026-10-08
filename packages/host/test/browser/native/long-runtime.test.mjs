import assert from 'node:assert/strict';
import { mkdtemp, mkdir, rm } from 'node:fs/promises';
import { tmpdir } from '../temp-root.mjs';
import path from 'node:path';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import test from 'node:test';
import { atomicPrivateJson } from '../../../src/browser/native/common.mjs';
import { doctor, request } from '../../../src/browser/client.mjs';

const profileId = '0123456789abcdef0123456789abcdef';
const extensionId = 'abcdefghijklmnopabcdefghijklmnop';
const frame = value => {
  const body = Buffer.from(JSON.stringify(value));
  const size = Buffer.alloc(4); size.writeUInt32LE(body.length);
  return Buffer.concat([size, body]);
};

test('migrated long data root connects native, Node and Python clients', { timeout: 10000 }, async () => {
  const temporary = await mkdtemp(path.join(tmpdir(), 'n-'));
  // Match the migrated installation, including its full 32-digit profile ID.
  const padding = 71 - Buffer.byteLength(temporary) - 1;
  assert.ok(padding > 0, 'Use a short task TMPDIR for the socket regression');
  const root = path.join(temporary, 'd'.repeat(padding));
  await mkdir(root);
  await atomicPrivateJson(path.join(root, 'config.json'), {
    schemaVersion: 1, dataRoot: root, extensionId, buildId: 'fixture', nodePath: process.execPath,
    providers: { feishu: { profileId, targetUrl: 'https://x.feishu.cn/next/', allowedOrigins: ['https://x.feishu.cn'] } },
  });
  const child = spawn(process.execPath, [new URL('../../../src/browser/native/native-host.mjs', import.meta.url).pathname, `chrome-extension://${extensionId}/`], { env: { ...process.env, OPSAIL_CHROME_DATA_ROOT: root } });
  let buffer = Buffer.alloc(0);
  const hello = new Promise(resolve => child.stdout.on('data', chunk => {
    buffer = Buffer.concat([buffer, chunk]);
    while (buffer.length >= 4 && buffer.length >= buffer.readUInt32LE(0) + 4) {
      const size = buffer.readUInt32LE(0);
      const message = JSON.parse(buffer.subarray(4, size + 4)); buffer = buffer.subarray(size + 4);
      if (message.type === 'request') child.stdin.write(frame({ type: 'response', requestId: message.requestId, ok: true, data: { profileId, ready: true } }));
      else resolve(message);
    }
  }));
  try {
    child.stdin.write(frame({ type: 'hello', protocolVersion: 1, extensionVersion: '0.1.0', buildId: 'fixture', profileId }));
    const reply = await hello;
    assert.equal(reply.type, 'hello-ok', JSON.stringify(reply));
    assert.deepEqual((await doctor(root)).profiles, [profileId]);
    assert.equal((await request('ping', undefined, {}, { dataRoot: root, profileId })).profileId, profileId);
    const code = `import sys
from pathlib import Path
sys.path.insert(0, sys.argv[1])
from chat import extension
extension.DEFAULT_DATA_ROOT = Path(sys.argv[2])
profile = sys.argv[3]
result = extension.request({'_provider': 'feishu', 'browser': {'transport': 'chrome-extension', 'profile_id': profile}}, 'status', {})
assert result['profileId'] == profile and result['ready']
print('python client connected')`;
    const python = spawn('python3', ['-c', code, new URL('../../../src/', import.meta.url).pathname, root, profileId]);
    let output = ''; python.stdout.on('data', chunk => { output += chunk; }); python.stderr.on('data', chunk => { output += chunk; });
    const [exitCode] = await once(python, 'close');
    assert.equal(exitCode, 0, output);
    assert.match(output, /python client connected/);
  } finally {
    child.kill(); await once(child, 'close');
    await rm(temporary, { recursive: true, force: true });
  }
});

import {feishuBuildNames} from '../extension/identity-builds.js';
import {boundedDiagnostics} from '../extension/diagnostics.js';
import { createConnection, createServer } from 'node:net';
import { promises as fs } from 'node:fs';
import path from 'node:path';
import {
  MAX_MESSAGE_BYTES, OPERATIONS, OpsailChromeError, PROTOCOL_VERSION, assertAllowedRequest,
  assertSafeProfileId, atomicPrivateJson, defaultDataRoot, encodeLine, ensurePrivateDir,
  bridgeDisabled, isObject, loadConfig, parseLine, runtimeSocketPath,
} from './common.mjs';

const dataRoot = process.env.OPSAIL_CHROME_DATA_ROOT || defaultDataRoot;
const origin = process.argv.at(-1);
let config;
let hello;
let nativeBuffer = Buffer.alloc(0);
let socketServer;
let socketPath;
let socketInode;
let metadataPath;
let metadataInode;
const pending = new Map();

function nativeFrame(value) {
  const payload = Buffer.from(JSON.stringify(value));
  if (payload.length > MAX_MESSAGE_BYTES) throw new OpsailChromeError('message-too-large', 'Native message exceeds 512 KiB.');
  const size = Buffer.alloc(4); size.writeUInt32LE(payload.length);
  process.stdout.write(Buffer.concat([size, payload]));
}

function nativeFail(code, message) {
  try { nativeFrame({ type: 'error', error: { code, message } }); } catch { /* stdout may be gone */ }
}

function expectedOrigin(extensionId) {
  return `chrome-extension://${extensionId}/`;
}

async function privateRuntime(profileId) {
  const runtime = path.join(dataRoot, 'runtime');
  await ensurePrivateDir(runtime);
  socketPath = runtimeSocketPath(dataRoot, profileId);
  metadataPath = path.join(runtime, `${profileId}.json`);
  try {
    const old = await fs.lstat(socketPath);
    if (old.isSymbolicLink()) throw new OpsailChromeError('unsafe-path', 'Refusing a symlinked runtime socket.');
    if (old.isSocket()) {
      const active = await new Promise((resolve) => {
        const client = createConnection(socketPath);
        client.once('connect', () => { client.destroy(); resolve(true); });
        client.once('error', () => resolve(false));
      });
      if (active) throw new OpsailChromeError('profile-active', 'An Opsail Chrome host is already active for this profile.');
      await fs.unlink(socketPath);
    } else throw new OpsailChromeError('unsafe-path', 'Runtime socket path is occupied.');
  } catch (error) { if (error?.code !== 'ENOENT') throw error; }
  await atomicPrivateJson(metadataPath, {
    schemaVersion: 1, profileId, socket: path.basename(socketPath), startedAt: new Date().toISOString(),
  });
  metadataInode = (await fs.lstat(metadataPath)).ino;
}

function injectBinding(request) {
  const builds = request.provider === 'feishu' ? {identityBuilds: feishuBuildNames(config)} : {};
  if (request.operation === 'verifyBinding') return {...request, binding: {...request.binding, ...builds}};
  if (!request.provider || request.operation === 'ping' || request.operation === 'pause') return request;
  const provider = config.providers[request.provider];
  if (!provider || provider.profileId !== hello.profileId || typeof provider.targetUrl !== 'string' || !Array.isArray(provider.allowedOrigins)) {
    throw new OpsailChromeError('not-bound', `No verified ${request.provider} binding exists for this Chrome profile.`);
  }
  const { targetUrl, allowedOrigins, accountHash, tenantHash } = provider;
  return { ...request, binding: { targetUrl, allowedOrigins, accountHash, tenantHash, ...builds } };
}

function settleAll(code, message) {
  for (const { reject, timer } of pending.values()) { clearTimeout(timer); reject(new OpsailChromeError(code, message)); }
  pending.clear();
}

async function forwardToExtension(request) {
  if (await bridgeDisabled(dataRoot)) throw new OpsailChromeError('bridge-disabled', 'Opsail Chrome bridge was uninstalled; reinstall before reading.');
  config = await loadConfig(dataRoot);
  if (config.buildId !== hello.buildId || config.extensionId !== hello.extensionId) {
    if (config.extensionId === hello.extensionId) nativeFrame({type: 'build-changed', buildId: config.buildId});
    throw new OpsailChromeError('build-changed', 'Installed Chrome build changed; reload the stable extension directory before reading.');
  }
  const bounded = injectBinding(assertAllowedRequest(request));
  return new Promise((resolve, reject) => {
    if (pending.has(bounded.requestId)) throw new OpsailChromeError('duplicate-request', 'A request with this requestId is already pending.');
    const timer = setTimeout(() => { pending.delete(bounded.requestId); reject(new OpsailChromeError('timeout', 'Extension did not respond within 60 seconds.')); }, 60_000);
    pending.set(bounded.requestId, { resolve, reject, timer });
    nativeFrame({ type: 'request', ...bounded });
  });
}

async function startSocket(profileId) {
  await ensurePrivateDir(path.join(dataRoot, 'runtime'));
  const lock = path.join(dataRoot, 'runtime/config.lock');
  try { await fs.mkdir(lock, {mode: 0o700}); }
  catch (error) { if (error.code === 'EEXIST') throw new OpsailChromeError('binding-busy', 'Configuration or runtime cleanup is in progress; reconnect.'); throw error; }
  try { await startSocketUnlocked(profileId); }
  finally { await fs.rmdir(lock); }
}

async function startSocketUnlocked(profileId) {
  await privateRuntime(profileId);
  socketServer = createServer((connection) => {
    let buffer = '';
    connection.setEncoding('utf8');
    connection.on('data', async (chunk) => {
      buffer += chunk;
      if (Buffer.byteLength(buffer) > MAX_MESSAGE_BYTES) { connection.end(encodeLine({ ok: false, error: { code: 'message-too-large', message: 'Request exceeds 512 KiB.' } })); return; }
      let index;
      while ((index = buffer.indexOf('\n')) >= 0) {
        const line = buffer.slice(0, index); buffer = buffer.slice(index + 1);
        try {
          const request = parseLine(line);
          const result = await forwardToExtension(request);
          connection.write(encodeLine({ type: 'response', protocolVersion: PROTOCOL_VERSION, requestId: request.requestId, ok: true, data: result }));
        } catch (error) {
          const requestId = (() => { try { const parsed = JSON.parse(line); return typeof parsed?.requestId === 'string' ? parsed.requestId : 'unknown'; } catch { return 'unknown'; } })();
          connection.write(encodeLine({ type: 'response', protocolVersion: PROTOCOL_VERSION, requestId, ok: false, error: { code: error.code || 'internal-error', message: error.message || 'Native host failed.', ...(error.diagnostics ? {diagnostics: boundedDiagnostics(error.diagnostics)} : {}) } }));
        }
      }
    });
    connection.on('error', () => {});
  });
  await new Promise((resolve, reject) => socketServer.once('error', reject).listen(socketPath, resolve));
  await fs.chmod(socketPath, 0o600);
  socketInode = (await fs.lstat(socketPath)).ino;
}

async function onNativeMessage(message) {
  if (!hello) {
    if (!isObject(message) || message.type !== 'hello' || message.protocolVersion !== PROTOCOL_VERSION ||
        message.extensionVersion !== '0.1.0' || typeof message.buildId !== 'string') {
      throw new OpsailChromeError('handshake-rejected', 'Extension hello does not match the installed protocol or build.');
    }
    const profileId = assertSafeProfileId(message.profileId);
    if (message.buildId !== config.buildId) {
      nativeFrame({type: 'build-changed', buildId: config.buildId});
      throw new OpsailChromeError('build-changed', 'Reload the stable extension directory to match the installed build.');
    }
    await startSocket(profileId);
    hello = { profileId, buildId: message.buildId, extensionId: config.extensionId };
    nativeFrame({ type: 'hello-ok', protocolVersion: PROTOCOL_VERSION, profileId });
    return;
  }
  if (!isObject(message) || message.type !== 'response' || typeof message.requestId !== 'string') {
    throw new OpsailChromeError('invalid-response', 'Extension sent an invalid response.');
  }
  const item = pending.get(message.requestId);
  if (!item) return;
  pending.delete(message.requestId); clearTimeout(item.timer);
  if (message.ok === true) item.resolve(message.data);
  else item.reject(new OpsailChromeError(message.error?.code || 'extension-error', message.error?.message || 'Extension rejected request.', message.error?.diagnostics));
}

async function stop() {
  settleAll('extension-disconnected', 'Chrome extension disconnected.');
  if (socketServer) await new Promise((resolve) => socketServer.close(resolve));
  if (socketPath) { try { if ((await fs.lstat(socketPath)).ino === socketInode) await fs.unlink(socketPath); } catch {} }
  if (metadataPath && metadataInode) { try { if ((await fs.lstat(metadataPath)).ino === metadataInode) await fs.unlink(metadataPath); } catch {} }
}

async function fatal(code, message) {
  nativeFail(code, message);
  process.stdin.pause();
  await stop();
  process.exitCode = 2;
  process.exit();
}

try {
  config = await loadConfig(dataRoot);
  if (await bridgeDisabled(dataRoot)) throw new OpsailChromeError('bridge-disabled', 'Opsail Chrome bridge was uninstalled; reinstall before connecting.');
  if (origin !== expectedOrigin(config.extensionId)) throw new OpsailChromeError('origin-denied', 'Native host caller is not the installed Opsail Chrome extension.');
  let stdinQueue = Promise.resolve();
  process.stdin.on('data', (chunk) => { stdinQueue = stdinQueue.then(async () => {
    nativeBuffer = Buffer.concat([nativeBuffer, chunk]);
    if (nativeBuffer.length > MAX_MESSAGE_BYTES + 4) return fatal('message-too-large', 'Native frame exceeds 512 KiB.');
    while (nativeBuffer.length >= 4) {
      const size = nativeBuffer.readUInt32LE(0);
      if (size > MAX_MESSAGE_BYTES) return fatal('message-too-large', 'Native frame exceeds 512 KiB.');
      if (nativeBuffer.length < size + 4) return;
      const payload = nativeBuffer.subarray(4, size + 4); nativeBuffer = nativeBuffer.subarray(size + 4);
      try { await onNativeMessage(JSON.parse(payload.toString('utf8'))); } catch (error) {
        if (!hello) return fatal(error.code || 'handshake-failed', error.message || 'Native host rejected message.');
        nativeFail(error.code || 'protocol-error', error.message || 'Native host rejected message.');
      }
    }
  }).catch((error) => fatal(error.code || 'native-host-failed', error.message || 'Native host failed.')); });
  process.stdin.on('end', () => { stop().finally(() => process.exit()); });
  process.on('SIGTERM', () => { stop().finally(() => process.exit()); });
} catch (error) {
  nativeFail(error.code || 'startup-failed', error.message || 'Could not start native host.');
  process.exitCode = 2;
}

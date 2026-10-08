import { createConnection } from 'node:net';
import { promises as fs } from 'node:fs';
import path from 'node:path';
import { randomUUID } from 'node:crypto';
import { MAX_MESSAGE_BYTES, OpsailChromeError, PROTOCOL_VERSION, assertAllowedRequest, assertSafeProfileId, bridgeDisabled, defaultDataRoot, encodeLine, loadConfig, parseLine, runtimeSocketPath } from './native/common.mjs';

export async function runtimeProfiles(dataRoot) {
  const runtime = path.join(dataRoot, 'runtime');
  try {
    const entries = await fs.readdir(runtime, { withFileTypes: true });
    const profiles = [], stale = [];
    if (entries.filter(entry => /^[a-f0-9]{1,32}\.json$/.test(entry.name)).length > 128) throw new OpsailChromeError('runtime-record-limit', 'Runtime profile inventory exceeds 128 records; review stale metadata before retrying.');
    for (const entry of entries) {
      if (!entry.name.endsWith('.json') || !entry.isFile()) continue;
      const profileId = entry.name.slice(0, -5);
      try { assertSafeProfileId(profileId); } catch { continue; }
      if (await profileOnline(dataRoot, profileId)) profiles.push(profileId);
      else stale.push(profileId);
    }
    return {profiles: profiles.sort(), stale: stale.sort()};
  } catch (error) { if (error.code === 'ENOENT') return {profiles: [], stale: []}; throw error; }
}

export async function profileOnline(dataRoot, profileId) {
  const socketPath = runtimeSocketPath(dataRoot, profileId);
  const info = await fs.lstat(socketPath).catch(() => null);
  if (!info?.isSocket() || info.uid !== process.getuid() || (info.mode & 0o077)) return false;
  return new Promise(resolve => {
    const socket = createConnection(socketPath);
    const timer = setTimeout(() => finish(false), 500);
    const finish = value => { clearTimeout(timer); socket.destroy(); resolve(value); };
    socket.once('connect', () => finish(true)); socket.once('error', () => finish(false));
  });
}

// Cleanup requires positive evidence of an absent/refused socket. A timeout,
// unsafe endpoint or permission error is unknown, never permission to delete.
export async function runtimeSocketState(dataRoot, profileId) {
  const socketPath = runtimeSocketPath(dataRoot, profileId);
  let info;
  try { info = await fs.lstat(socketPath); }
  catch (error) { return error.code === 'ENOENT' ? 'offline' : 'unknown'; }
  if (!info.isSocket() || info.uid !== process.getuid() || (info.mode & 0o077)) return 'unknown';
  return new Promise(resolve => {
    const socket = createConnection(socketPath);
    const timer = setTimeout(() => finish('unknown'), 500);
    const finish = value => { clearTimeout(timer); socket.destroy(); resolve(value); };
    socket.once('connect', () => finish('live'));
    socket.once('error', error => finish(['ENOENT', 'ECONNREFUSED'].includes(error.code) ? 'offline' : 'unknown'));
  });
}

export async function request(operation, provider, args = {}, options = {}) {
  const dataRoot = options.dataRoot || defaultDataRoot;
  if (await bridgeDisabled(dataRoot)) throw new OpsailChromeError('bridge-disabled', 'Opsail Chrome bridge was uninstalled; reinstall before reading.');
  await loadConfig(dataRoot);
  let profileId = options.profileId;
  if (profileId) assertSafeProfileId(profileId);
  else {
    const {profiles} = await runtimeProfiles(dataRoot);
    if (profiles.length !== 1) throw new OpsailChromeError(profiles.length ? 'profile-required' : 'extension-offline', profiles.length ? 'Multiple Chrome profiles are connected; specify profileId.' : 'No connected Opsail Chrome extension profile.');
    [profileId] = profiles;
  }
  const socketPath = runtimeSocketPath(dataRoot, profileId);
  const requestId = randomUUID();
  const payload = assertAllowedRequest({ protocolVersion: PROTOCOL_VERSION, requestId, operation, ...(provider ? { provider } : {}), args, ...(options.binding ? {binding: options.binding} : {}) });
  return new Promise((resolve, reject) => {
    const socket = createConnection(socketPath);
    let buffer = '';
    const timer = setTimeout(() => { socket.destroy(); reject(new OpsailChromeError('timeout', 'Native host did not respond within its deadline.')); }, options.timeoutMs ?? 60_000);
    socket.on('end', () => { clearTimeout(timer); reject(new OpsailChromeError('extension-offline', 'Native host closed before a complete reply.')); });
    socket.on('connect', () => socket.write(encodeLine(payload)));
    socket.on('data', (chunk) => {
      buffer += chunk;
      if (Buffer.byteLength(buffer) > MAX_MESSAGE_BYTES) { clearTimeout(timer); socket.destroy(); reject(new OpsailChromeError('message-too-large', 'Native host response exceeds 512 KiB.')); return; }
      const index = buffer.indexOf('\n'); if (index < 0) return;
      try {
        const response = parseLine(buffer.slice(0, index)); clearTimeout(timer); socket.end();
        if (response.type !== 'response' || response.protocolVersion !== PROTOCOL_VERSION || response.requestId !== requestId) throw new OpsailChromeError('protocol-error', 'Native host response did not match request.');
        if (response.ok !== true) {
          const codes = ['build-changed', 'implementation-changed', 'site-permission-required', 'tab-not-prepared', 'identity-unverified', 'unqualified-build', 'provider-degraded', 'target-tab-conflict', 'identity-read-failed', 'login-required', 'build-not-observed', 'tab-missing', 'document-changed', 'page-changed', 'binding-changed', 'load-timeout', 'injection-unavailable', 'account-changed', 'tenant-changed', 'identity-changed', 'paused', 'source-busy', 'extension-offline', 'bridge-disabled', 'not-bound', 'timeout'];
          const code = codes.includes(response.error?.code) ? response.error.code : 'native-host-error';
          throw new OpsailChromeError(code, `Opsail Chrome rejected the request (${code}); run doctor for the next step.`, response.error?.diagnostics);
        }
        resolve(response.data);
      } catch (error) { clearTimeout(timer); socket.destroy(); reject(error); }
    });
    socket.on('error', (error) => { clearTimeout(timer); reject(new OpsailChromeError('extension-offline', 'Cannot connect to the bound Opsail Chrome extension.')); });
  });
}

export async function doctor(dataRoot = defaultDataRoot) {
  const config = await loadConfig(dataRoot, { optional: true });
  const disabled = await bridgeDisabled(dataRoot);
  const inventory = disabled ? {profiles: [], stale: []} : await runtimeProfiles(dataRoot);
  return { schemaVersion: PROTOCOL_VERSION, configured: Boolean(config), disabled: Boolean(disabled), dataRoot, extensionId: config?.extensionId ?? null, buildId: config?.buildId ?? null, profiles: inventory.profiles, staleRuntimeRecords: inventory.stale.length };
}

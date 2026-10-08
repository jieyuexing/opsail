import { createHash, randomUUID } from 'node:crypto';
import { promises as fs } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import {FEISHU_BUILD, validIdentityBuilds} from '../extension/identity-builds.js';
import {boundedDiagnostics} from '../extension/diagnostics.js';
import {validateBinding} from '../extension/protocol.js';
import { resolveOpsailLayout } from '../../layout.js';

export const PROTOCOL_VERSION = 1;
export const MAX_MESSAGE_BYTES = 512 * 1024;
export const PROFILE_ID = /^[a-f0-9]{1,32}$/;
export const PROVIDERS = new Set(['feishu', 'teams']);
export const OPERATIONS = new Set([
  'ping', 'diagnose', 'verifyBinding', 'verifyBuild', 'status', 'prepare', 'select', 'catalog', 'read', 'qualify',
  'catalogPage', 'messagesPage', 'scrollBack', 'configure', 'pause',
]);

const browserRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
export const pluginRoot = path.resolve(browserRoot, '../..');
export const opsailLayout = resolveOpsailLayout();
export const repoRoot = opsailLayout.root;
export const defaultDataRoot = opsailLayout.retainedChatDataDir;
export const defaultBindingFile = opsailLayout.chatBindingFile;

export class OpsailChromeError extends Error {
  constructor(code, message, diagnostics) {
    super(message);
    this.code = code;
    if (diagnostics) this.diagnostics = boundedDiagnostics(diagnostics);
  }
}

export function jsonError(code, message) {
  return { schemaVersion: PROTOCOL_VERSION, ok: false, error: { code, message } };
}

export function isObject(value) {
  return Boolean(value) && typeof value === 'object' && !Array.isArray(value);
}

export function assertSafeProfileId(profileId) {
  if (typeof profileId !== 'string' || !PROFILE_ID.test(profileId)) {
    throw new OpsailChromeError('invalid-profile', 'profileId must contain 1 to 32 lowercase hexadecimal characters.');
  }
  return profileId;
}

// macOS allows 103 path bytes before the terminating NUL. Preserve the
// old socket name when it fits; otherwise encode the complete ID losslessly.
export function runtimeSocketPath(dataRoot, profileId) {
  assertSafeProfileId(profileId);
  const runtime = path.join(dataRoot, 'runtime');
  const legacy = path.join(runtime, `${profileId}.sock`);
  if (Buffer.byteLength(legacy) <= 103) return legacy;
  // The leading nibble preserves both leading zeroes and odd-length IDs.
  let hex = `1${profileId}`;
  if (hex.length % 2) hex = `0${hex}`;
  const compact = path.join(runtime, Buffer.from(hex, 'hex').toString('base64url'));
  if (Buffer.byteLength(compact) > 103) throw new OpsailChromeError('socket-path-too-long', 'Opsail data root is too long for a private Unix socket.');
  return compact;
}

export function assertAllowedRequest(request) {
  if (!isObject(request) || request.protocolVersion !== PROTOCOL_VERSION ||
      typeof request.requestId !== 'string' || request.requestId.length < 1 || request.requestId.length > 128 ||
      !OPERATIONS.has(request.operation) || !isObject(request.args ?? {}) ||
      !Object.keys(request).every((key) => ['protocolVersion', 'requestId', 'operation', 'provider', 'args', 'binding'].includes(key))) {
    throw new OpsailChromeError('invalid-request', 'Request does not match the Opsail Chrome protocol.');
  }
  if (request.provider !== undefined && !PROVIDERS.has(request.provider)) {
    throw new OpsailChromeError('invalid-provider', 'Provider must be feishu or teams.');
  }
  if (['diagnose', 'verifyBinding', 'verifyBuild', 'prepare', 'select', 'catalog', 'read', 'qualify', 'catalogPage', 'messagesPage', 'scrollBack', 'pause', 'configure'].includes(request.operation) && !request.provider) {
    throw new OpsailChromeError('provider-required', 'This operation requires a provider.');
  }
  if (request.operation === 'verifyBinding') {
    if (!isObject(request.binding) || Object.keys(request.binding).some(key => !['targetUrl', 'allowedOrigins', 'accountHash', 'tenantHash'].includes(key))) throw new OpsailChromeError('invalid-binding', 'Binding probe only accepts fixed site coordinates and identity hashes.');
    validateBinding(request.provider, request.binding);
  } else if ('binding' in request) throw new OpsailChromeError('invalid-request', 'Only an explicit binding probe accepts candidate site coordinates.');
  const permitted = {
    ping: [], diagnose: [], verifyBinding: [], verifyBuild: ['build'], status: [], configure: [], prepare: [], qualify: [], pause: ['paused'],
    select: ['conversationId', 'conversationName'], catalog: ['limit'],
    read: ['conversationId', 'conversationName', 'conversationUrl', 'limit'],
    catalogPage: ['cursor', 'limit'], messagesPage: ['conversationId', 'cursor', 'limit', 'rangeStart', 'rangeEnd'],
    scrollBack: ['conversationId', 'conversationName', 'direction'],
  }[request.operation];
  if (!Object.keys(request.args).every((key) => permitted.includes(key))) throw new OpsailChromeError('invalid-request', 'Request arguments are not permitted for this operation.');
  if (request.operation === 'verifyBuild' && (request.provider !== 'feishu' || typeof request.args.build !== 'string' || !FEISHU_BUILD.test(request.args.build))) throw new OpsailChromeError('invalid-build', 'Use an exact Feishu index.<hex>.js bundle filename.');
  if ('limit' in request.args && (!Number.isInteger(request.args.limit) || request.args.limit < 1 || request.args.limit > (['catalog', 'catalogPage'].includes(request.operation) ? 100 : 200))) throw new OpsailChromeError('invalid-limit', 'Request limit is outside its bounded page size.');
  if ('paused' in request.args && typeof request.args.paused !== 'boolean') throw new OpsailChromeError('invalid-request', 'paused must be a boolean.');
  if ('direction' in request.args && !['up', 'bottom'].includes(request.args.direction)) throw new OpsailChromeError('invalid-request', 'direction must be up or bottom.');
  for (const [key, value] of Object.entries(request.args)) if (!['limit', 'paused'].includes(key) && (typeof value !== 'string' || !value || value.length > (key === 'cursor' ? 8192 : 2048) || /[\x00-\x1f]/.test(value))) throw new OpsailChromeError('invalid-request', 'Request text argument is invalid.');
  return request;
}

export function encodeLine(value) {
  const line = `${JSON.stringify(value)}\n`;
  if (Buffer.byteLength(line) > MAX_MESSAGE_BYTES) {
    throw new OpsailChromeError('message-too-large', 'Protocol message exceeds 512 KiB.');
  }
  return line;
}

export function parseLine(line) {
  if (Buffer.byteLength(line) > MAX_MESSAGE_BYTES) {
    throw new OpsailChromeError('message-too-large', 'Protocol message exceeds 512 KiB.');
  }
  try {
    return JSON.parse(line);
  } catch {
    throw new OpsailChromeError('invalid-json', 'Protocol message is not valid JSON.');
  }
}

export async function ensurePrivateDir(dir) {
  const absolute = path.resolve(dir);
  let current = path.parse(absolute).root;
  for (const segment of absolute.slice(current.length).split(path.sep).filter(Boolean)) {
    current = path.join(current, segment);
    try {
      const stat = await fs.lstat(current);
      if (stat.isSymbolicLink()) throw new OpsailChromeError('unsafe-path', `Refusing symlinked private path: ${current}`);
    } catch (error) {
      if (error?.code !== 'ENOENT') throw error;
      break;
    }
  }
  const parent = path.dirname(dir);
  await fs.mkdir(parent, { recursive: true, mode: 0o700 });
  try {
    const existing = await fs.lstat(dir);
    if (existing.isSymbolicLink() || !existing.isDirectory()) {
      throw new OpsailChromeError('unsafe-path', `Refusing unsafe private directory: ${dir}`);
    }
  } catch (error) {
    if (error?.code !== 'ENOENT') throw error;
    await fs.mkdir(dir, { mode: 0o700 });
  }
  await fs.chmod(dir, 0o700);
}

export async function readPrivateJson(file, { optional = false } = {}) {
  try {
    const stat = await fs.lstat(file);
    if (stat.isSymbolicLink() || !stat.isFile()) throw new OpsailChromeError('unsafe-path', `Refusing unsafe private file: ${file}`);
    if (stat.uid !== process.getuid() || (stat.mode & 0o777) !== 0o600) throw new OpsailChromeError('unsafe-path', `Private file permissions are unsafe: ${file}`);
    if (stat.size > MAX_MESSAGE_BYTES) throw new OpsailChromeError('config-too-large', 'Private configuration exceeds 512 KiB.');
    return JSON.parse(await fs.readFile(file, 'utf8'));
  } catch (error) {
    if (optional && error?.code === 'ENOENT') return null;
    if (error instanceof OpsailChromeError) throw error;
    throw new OpsailChromeError('invalid-config', `Could not read private configuration: ${error.message}`);
  }
}

export async function readJsonFile(file) {
  const stat = await fs.lstat(file);
  if (stat.isSymbolicLink() || !stat.isFile() || stat.size > MAX_MESSAGE_BYTES) throw new OpsailChromeError('unsafe-path', `Refusing unsafe JSON file: ${file}`);
  try { return JSON.parse(await fs.readFile(file, 'utf8')); } catch { throw new OpsailChromeError('invalid-json', `Invalid JSON file: ${file}`); }
}

export async function atomicPrivateJson(file, data) {
  await ensurePrivateDir(path.dirname(file));
  try {
    const stat = await fs.lstat(file);
    if (stat.isSymbolicLink()) throw new OpsailChromeError('unsafe-path', `Refusing symlink: ${file}`);
  } catch (error) {
    if (error?.code !== 'ENOENT') throw error;
  }
  const temp = path.join(path.dirname(file), `.${path.basename(file)}.${randomUUID()}.tmp`);
  await fs.writeFile(temp, `${JSON.stringify(data, null, 2)}\n`, { mode: 0o600 });
  await fs.chmod(temp, 0o600);
  await fs.rename(temp, file);
}

export async function loadConfig(dataRoot = defaultDataRoot, { optional = false } = {}) {
  const config = await readPrivateJson(path.join(dataRoot, 'config.json'), { optional });
  if (config === null) return null;
  if (!isObject(config) || config.schemaVersion !== 1 || config.dataRoot !== dataRoot ||
      typeof config.extensionId !== 'string' || typeof config.buildId !== 'string' ||
      typeof config.nodePath !== 'string' || !isObject(config.providers)) {
    throw new OpsailChromeError('invalid-config', 'Opsail Chrome configuration does not match schemaVersion 1.');
  }
  if (config.identityBuilds !== undefined && !validIdentityBuilds(config.identityBuilds)) throw new OpsailChromeError('invalid-config', 'Invalid identity build allow-list.');
  return config;
}

export async function bridgeDisabled(dataRoot = defaultDataRoot) {
  return readPrivateJson(path.join(dataRoot, 'runtime', 'disabled.json'), { optional: true });
}

export async function hashTree(root) {
  const info = await fs.lstat(root);
  if (!info.isDirectory() || info.isSymbolicLink()) throw new OpsailChromeError('unsafe-path', 'Extension tree root must be a real directory.');
  const hash = createHash('sha256');
  async function visit(relative) {
    const absolute = path.join(root, relative);
    const entries = await fs.readdir(absolute, { withFileTypes: true });
    for (const entry of entries.sort((a, b) => a.name.localeCompare(b.name))) {
      const child = path.join(relative, entry.name);
      if (entry.isSymbolicLink()) throw new OpsailChromeError('unsafe-path', `Extension release contains a symlink: ${child}`);
      if (entry.isDirectory()) await visit(child);
      else if (entry.isFile() && child !== 'build.json' && child !== 'build.js') {
        hash.update(child); hash.update('\0'); hash.update(await fs.readFile(path.join(root, child))); hash.update('\0');
      }
    }
  }
  await visit('');
  return hash.digest('hex');
}

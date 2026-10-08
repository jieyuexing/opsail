import {FEISHU_BUILD, defaultIdentityBuilds, MAX_IDENTITY_BUILDS} from './extension/identity-builds.js';
import {boundedDiagnostics, identityNextStep} from './extension/diagnostics.js';
import { promises as fs } from 'node:fs';
import os from 'node:os';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import path from 'node:path';
import { createHash } from 'node:crypto';
import {
  OpsailChromeError, PROTOCOL_VERSION, bridgeDisabled, atomicPrivateJson, defaultBindingFile, defaultDataRoot, ensurePrivateDir,
  hashTree, isObject, loadConfig, pluginRoot, readJsonFile, readPrivateJson,
} from './native/common.mjs';
import { doctor as bridgeDoctor, profileOnline, runtimeProfiles, request } from './client.mjs';

const nativeHostName = 'com.opsail.chrome';
async function secureReleaseTree(directory) {
  await fs.chmod(directory, 0o700);
  for (const entry of await fs.readdir(directory, { withFileTypes: true })) {
    const child = path.join(directory, entry.name);
    if (entry.isSymbolicLink()) throw new OpsailChromeError('unsafe-path', 'Extension release contains a symlink.');
    if (entry.isDirectory()) await secureReleaseTree(child);
    else if (entry.isFile()) await fs.chmod(child, 0o600);
    else throw new OpsailChromeError('unsafe-path', 'Extension release contains a non-file entry.');
  }
}

export function chromeNativeManifestPath(home = os.homedir()) {
  return path.join(home, 'Library', 'Application Support', 'Google', 'Chrome', 'NativeMessagingHosts', `${nativeHostName}.json`);
}

async function copyExtension(source, release) {
  const stat = await fs.lstat(source).catch(() => null);
  if (!stat?.isDirectory() || stat.isSymbolicLink()) throw new OpsailChromeError('extension-missing', 'Browser extension source is unavailable.');
  const buildId = await hashTree(source);
  const destination = path.join(release, buildId, 'extension');
  await ensurePrivateDir(path.dirname(destination));
  let existing = false;
  try {
    const prior = await fs.lstat(destination);
    if (prior.isSymbolicLink() || !prior.isDirectory()) throw new OpsailChromeError('unsafe-path', 'Extension release path is unsafe.');
    existing = true;
  } catch (error) {
    if (error.code !== 'ENOENT') throw error;
    await ensurePrivateDir(path.dirname(destination));
    await fs.cp(source, destination, { recursive: true, dereference: false, errorOnExist: true });
    await fs.chmod(destination, 0o700);
  }
  if (existing && await hashTree(destination) !== buildId) {
    throw new OpsailChromeError('release-tampered', 'Existing extension release does not match its build identity.');
  }
  await secureReleaseTree(destination);
  const manifest = await readJsonFile(path.join(destination, 'manifest.json'));
  if (typeof manifest.key !== 'string' || !manifest.key) {
    throw new OpsailChromeError('extension-id-unavailable', 'Extension manifest must contain its fixed public key.');
  }
  const digest = createHash('sha256').update(Buffer.from(manifest.key, 'base64')).digest().subarray(0, 16);
  const extensionId = [...digest].map((byte) => byte.toString(16).padStart(2, '0')).join('').replace(/[0-9a-f]/g, (hex) => String.fromCharCode(97 + Number.parseInt(hex, 16)));
  const build = { protocolVersion: PROTOCOL_VERSION, extensionVersion: '0.1.0', buildId };
  await atomicPrivateJson(path.join(destination, 'build.json'), build);
  // The worker imports this module, so a cached stale worker reports its own
  // (old) build and the native host's build-changed signal reloads it.
  const module = path.join(destination, 'build.js');
  await fs.writeFile(module + '.tmp', `export const BUILD = Object.freeze(${JSON.stringify(build)})\n`, { mode: 0o600 });
  await fs.rename(module + '.tmp', module);
  return { ...build, extensionId, unpackedPath: destination };
}

// Chrome always loads this real directory. Exchange complete trees without a
// missing-path window; unsupported filesystems/platforms fail before replacement.
async function publishCurrent(dataRoot, release) {
  const parent = path.join(dataRoot, 'runtime', 'current');
  await ensurePrivateDir(parent);
  const destination = path.join(parent, 'extension');
  const prior = await fs.lstat(destination).catch(error => { if (error.code !== 'ENOENT') throw error; return null; });
  if (prior && (!prior.isDirectory() || prior.isSymbolicLink())) throw new OpsailChromeError('unsafe-path', 'Stable extension directory is unsafe.');
  if (prior) await hashTree(destination); // Refuse nested symlinks too.
  const staging = await fs.mkdtemp(path.join(parent, '.extension-'));
  try {
    await fs.cp(release.unpackedPath, staging, {recursive: true, dereference: false});
    await secureReleaseTree(staging);
    if (await hashTree(staging) !== release.buildId) throw new OpsailChromeError('release-tampered', 'Staged extension build changed.');
    if (prior) {
      try { await promisify(execFile)('python3', [path.join(pluginRoot, 'src/browser/swap-directory.py'), staging, destination], {timeout: 10_000}); }
      catch { throw new OpsailChromeError('atomic-install-unavailable', 'Could not atomically replace the stable extension directory; previous directory retained.'); }
    } else await fs.rename(staging, destination);
  } finally { await fs.rm(staging, {recursive: true, force: true}); }
  return destination;
}

async function retainReleases(dataRoot, buildId, count = 3) {
  const root = path.join(dataRoot, 'runtime', 'releases');
  const entries = [];
  for (const entry of await fs.readdir(root, {withFileTypes: true})) {
    if (!/^[a-f0-9]{64}$/.test(entry.name)) continue;
    if (!entry.isDirectory() || entry.isSymbolicLink()) throw new OpsailChromeError('unsafe-path', 'Release retention found an unsafe directory.');
    const dir = path.join(root, entry.name);
    if (await hashTree(path.join(dir, 'extension')) !== entry.name) throw new OpsailChromeError('release-tampered', 'Refusing retention of an unverified release.');
    entries.push({dir, name: entry.name, time: (await fs.stat(dir)).mtimeMs});
  }
  entries.sort((a, b) => Number(b.name === buildId) - Number(a.name === buildId) || b.time - a.time || a.name.localeCompare(b.name));
  for (const entry of entries.slice(count)) await fs.rm(entry.dir, {recursive: true});
}

function defaultConfig(dataRoot, release) {
  return {
    schemaVersion: 1, dataRoot, extensionId: release.extensionId, buildId: release.buildId,
    nodePath: process.execPath, providers: {}, identityBuilds: defaultIdentityBuilds(),
  };
}

async function writeNativeManifest(config, { home = os.homedir(), writeRegistry = true } = {}) {
  const hostScript = path.join(pluginRoot, 'src', 'browser', 'native', 'native-host.mjs');
  const stat = await fs.lstat(hostScript).catch(() => null);
  if (!stat?.isFile() || stat.isSymbolicLink()) throw new OpsailChromeError('native-host-missing', 'Native host executable is unavailable.');
  const shellQuote = (value) => `'${value.replaceAll("'", "'\\\"'\\\"'")}'`;
  const wrapper = path.join(config.dataRoot, 'runtime', 'native-messaging', 'opsail-native-host');
  await ensurePrivateDir(path.dirname(wrapper));
  try {
    if ((await fs.lstat(wrapper)).isSymbolicLink()) throw new OpsailChromeError('unsafe-path', 'Native host wrapper path is a symlink.');
  } catch (error) { if (error.code !== 'ENOENT') throw error; }
  await fs.writeFile(wrapper, `#!/bin/sh\nset -eu\nexport OPSAIL_CHROME_DATA_ROOT=${shellQuote(config.dataRoot)}\nexec ${shellQuote(config.nodePath)} ${shellQuote(hostScript)} "$@"\n`, { mode: 0o700 });
  await fs.chmod(wrapper, 0o700);
  const manifest = {
    name: nativeHostName,
    description: 'Opsail Chrome local read bridge',
    path: wrapper,
    type: 'stdio',
    allowed_origins: [`chrome-extension://${config.extensionId}/`],
  };
  const staged = path.join(config.dataRoot, 'runtime', 'native-messaging', `${nativeHostName}.json`);
  await atomicPrivateJson(staged, manifest);
  if (writeRegistry) await atomicPrivateJson(chromeNativeManifestPath(home), manifest);
  return { staged, registry: writeRegistry ? chromeNativeManifestPath(home) : null };
}

async function installUnlocked({ dataRoot = defaultDataRoot, extensionSource = path.join(pluginRoot, 'src', 'browser', 'extension'), home, writeRegistry = true } = {}) {
  await ensurePrivateDir(dataRoot);
  for (const child of ['jobs', 'receipts', 'runtime']) await ensurePrivateDir(path.join(dataRoot, child));
  const disabled = path.join(dataRoot, 'runtime', 'disabled.json');
  try {
    const stat = await fs.lstat(disabled);
    if (stat.isSymbolicLink() || !stat.isFile()) throw new OpsailChromeError('unsafe-path', 'Bridge disabled marker is unsafe.');
    await fs.unlink(disabled);
  } catch (error) { if (error.code !== 'ENOENT') throw error; }
  const release = await copyExtension(extensionSource, path.join(dataRoot, 'runtime', 'releases'));
  const unpackedPath = await publishCurrent(dataRoot, release);
  const current = await loadConfig(dataRoot, { optional: true });
  if (current && (current.extensionId !== release.extensionId || current.buildId !== release.buildId)) {
    // A new release deliberately preserves bindings, but never silently changes their identity.
    current.extensionId = release.extensionId; current.buildId = release.buildId; current.nodePath = process.execPath;
    await atomicPrivateJson(path.join(dataRoot, 'config.json'), current);
  } else if (!current) {
    await atomicPrivateJson(path.join(dataRoot, 'config.json'), defaultConfig(dataRoot, release));
  }
  const config = await loadConfig(dataRoot);
  const native = await writeNativeManifest(config, { home, writeRegistry });
  await retainReleases(dataRoot, release.buildId);
  return { schemaVersion: PROTOCOL_VERSION, operation: 'install', dataRoot, ...release, unpackedPath, retainedReleases: 3, nativeManifest: native };
}

export async function install(options = {}) {
  const dataRoot = options.dataRoot ?? defaultDataRoot;
  await ensurePrivateDir(path.join(dataRoot, 'runtime'));
  const lock = path.join(dataRoot, 'runtime', 'config.lock');
  try { await fs.mkdir(lock, {mode: 0o700}); }
  catch (error) { if (error.code === 'EEXIST') throw new OpsailChromeError('install-busy', 'Another install holds runtime/config.lock; confirm it has stopped before removing that lock.'); throw error; }
  try { return await installUnlocked(options); }
  finally { await fs.rmdir(lock); }
}

function checkedHash(value, name) {
  if (typeof value !== 'string' || !/^[a-f0-9]{64}$/.test(value)) throw new OpsailChromeError('identity-unverified', `${name} was not verified by the extension.`);
  return value;
}

export async function bind(provider, {profileId, rebind = false, dataRoot = defaultDataRoot, sourceBindingFile = defaultBindingFile} = {}) {
  if (!['feishu', 'teams'].includes(provider)) throw new OpsailChromeError('invalid-provider', 'bind supports feishu or teams.');
  // Serialize binding writers. No candidate is ever published as a pending binding.
  await ensurePrivateDir(path.join(dataRoot, 'runtime'));
  const lock = path.join(dataRoot, 'runtime', 'config.lock');
  try { await fs.mkdir(lock, {mode: 0o700}); }
  catch (error) { if (error.code === 'EEXIST') throw new OpsailChromeError('binding-busy', 'Another binding operation is in progress.'); throw error; }
  try {
    const source = await readPrivateJson(sourceBindingFile);
    const config = await loadConfig(dataRoot);
    const originalConfig = JSON.stringify(config), originalSource = JSON.stringify(source);
    const prior = config.providers[provider];
    const browser = source?.providers?.[provider]?.browser;
    const stored = prior?.authoritative === true ? prior : browser?.transport === 'chrome-extension' ? {
      profileId: browser.profile_id, targetUrl: browser.target_url, allowedOrigins: browser.allowed_origins,
      accountHash: browser.account_hash, tenantHash: browser.tenant_hash,
    } : prior;
    const coordinates = prior?.authoritative === true ? prior : {targetUrl: browser?.target_url, allowedOrigins: browser?.allowed_origins};
    if (typeof coordinates.targetUrl !== 'string' || !Array.isArray(coordinates.allowedOrigins)) throw new OpsailChromeError('binding-unavailable', 'No existing browser site coordinates are available to import.');
    if (rebind) {
      checkedHash(stored?.accountHash, 'accountHash'); checkedHash(stored?.tenantHash, 'tenantHash');
      if (await profileOnline(dataRoot, stored.profileId)) throw new OpsailChromeError('bound-profile-online', 'The bound profile is still online; use it instead of rebinding.');
      const {profiles} = await runtimeProfiles(dataRoot);
      if (profiles.length !== 1) throw new OpsailChromeError(profiles.length ? 'ambiguous-profiles' : 'extension-offline', 'Rebind requires exactly one online replacement profile. Close other Opsail profile connections or start the intended profile, then retry.');
      if (profileId && profileId !== profiles[0]) throw new OpsailChromeError('profile-mismatch', 'The requested profile is not the sole online replacement.');
      [profileId] = profiles;
    }
    if (!profileId) throw new OpsailChromeError('profile-required', 'bind requires --profile-id, or --rebind for a verified offline binding.');
    const candidate = {targetUrl: coordinates.targetUrl, allowedOrigins: coordinates.allowedOrigins,
      ...(rebind ? {accountHash: stored.accountHash, tenantHash: stored.tenantHash} : {})};
    const status = await request('verifyBinding', provider, {}, {profileId, dataRoot, binding: candidate});
    if (!isObject(status) || status.ready !== true || status.profileId !== profileId) throw new OpsailChromeError('identity-unverified', 'Extension did not verify the requested profile and identity.', status?.diagnostics);
    const accountHash = checkedHash(status.accountHash, 'accountHash');
    const tenantHash = checkedHash(status.tenantHash, 'tenantHash');
    if (rebind && (accountHash !== stored.accountHash || tenantHash !== stored.tenantHash)) throw new OpsailChromeError('identity-mismatch', 'Replacement profile does not match the stored account and tenant; no binding changed.');
    if (rebind) {
      const {profiles} = await runtimeProfiles(dataRoot);
      if (await profileOnline(dataRoot, stored.profileId) || profiles.length !== 1 || profiles[0] !== profileId) throw new OpsailChromeError('profiles-changed', 'Online profiles changed during verification; retry rebind.');
    }
    if (JSON.stringify(await loadConfig(dataRoot)) !== originalConfig || JSON.stringify(await readPrivateJson(sourceBindingFile)) !== originalSource) throw new OpsailChromeError('binding-changed', 'Private configuration changed during verification; no binding changed.');
    // One atomic authority: chat/collection/native readers all consume this record.
    // Legacy browser coordinates and every snapshot root remain untouched.
    config.providers[provider] = {profileId, pending: false, authoritative: true, ...candidate, accountHash, tenantHash};
    await atomicPrivateJson(path.join(dataRoot, 'config.json'), config);
    return {schemaVersion: PROTOCOL_VERSION, operation: rebind ? 'rebind' : 'bind', provider, profileId, identityVerified: true, bindingAuthority: 'config', diagnostics: boundedDiagnostics(status.diagnostics)};
  } finally { await fs.rmdir(lock); }
}

// Build qualification is separate from history/API qualification. It can only
// extend this fixed identity method's filename gate, never enable an API driver.
export async function qualify(provider, {build, byOperator = false, dataRoot = defaultDataRoot} = {}) {
  if (provider !== 'feishu') throw new OpsailChromeError('invalid-provider', 'Build qualification supports feishu only.');
  if (typeof build !== 'string' || !FEISHU_BUILD.test(build)) throw new OpsailChromeError('invalid-build', 'Use one exact index.<6-64 lowercase hex>.js filename; paths, URLs and patterns are forbidden.');
  await ensurePrivateDir(path.join(dataRoot, 'runtime'));
  const lock = path.join(dataRoot, 'runtime/config.lock');
  try { await fs.mkdir(lock, {mode: 0o700}); }
  catch (error) { if (error.code === 'EEXIST') throw new OpsailChromeError('binding-busy', 'Another configuration operation is in progress.'); throw error; }
  try {
    const config = await loadConfig(dataRoot), original = JSON.stringify(config);
    const identityBuilds = config.identityBuilds ?? defaultIdentityBuilds();
    const existing = identityBuilds.feishu.find(entry => entry.name === build);
    if (existing) return {operation: 'qualify', provider, build, added: false, qualification: existing.qualification, behavioralContractObserved: existing.qualification === 'behavior-verified'};
    if (identityBuilds.feishu.length >= MAX_IDENTITY_BUILDS) throw new OpsailChromeError('build-limit', 'Identity build allow-list is full.');
    let diagnostics = {};
    if (!byOperator) {
      const bound = config.providers.feishu;
      if (!bound || !await profileOnline(dataRoot, bound.profileId)) throw new OpsailChromeError('bound-profile-offline', 'Cannot observe the contract on the bound profile. Start it, or explicitly qualify the reviewed exact filename with --by-operator, then bind --rebind.');
      checkedHash(bound.accountHash, 'accountHash'); checkedHash(bound.tenantHash, 'tenantHash');
      const observed = await request('verifyBuild', provider, {build}, {dataRoot, profileId: bound.profileId});
      diagnostics = boundedDiagnostics(observed?.diagnostics);
      if (observed?.ready !== true || observed?.profileId !== bound.profileId || observed.accountHash !== bound.accountHash || observed.tenantHash !== bound.tenantHash || diagnostics.behavioralContractPassed !== true || !diagnostics.bundleNames?.includes(build)) throw new OpsailChromeError('identity-unverified', 'Build qualification did not verify the bound identity and requested bundle.', diagnostics);
    }
    if (JSON.stringify(await loadConfig(dataRoot)) !== original) throw new OpsailChromeError('binding-changed', 'Configuration changed during qualification; no build was added.');
    const qualification = byOperator ? 'qualified-by-operator' : 'behavior-verified';
    config.identityBuilds = {feishu: [...identityBuilds.feishu, {name: build, qualification}]};
    await atomicPrivateJson(path.join(dataRoot, 'config.json'), config);
    return {operation: 'qualify', provider, build, added: true, qualification, behavioralContractObserved: !byOperator, diagnostics,
      note: byOperator ? 'Operator-qualified filename only; the behavioral contract was NOT observed. Every subsequent bind/read still requires the session-user contract and bound identity hashes.' : 'Observed the session-user contract and matching bound identity; history/API remains unqualified.'};
  } finally { await fs.rmdir(lock); }
}

export async function uninstall({ dataRoot = defaultDataRoot, home, removeRegistry = true } = {}) {
  const registry = chromeNativeManifestPath(home);
  if (removeRegistry) {
    try {
      const stat = await fs.lstat(registry);
      if (stat.isSymbolicLink() || !stat.isFile()) throw new OpsailChromeError('unsafe-path', 'Native messaging registration is unsafe.');
      const manifest = await readPrivateJson(registry);
      if (manifest.name !== nativeHostName || manifest.description !== 'Opsail Chrome local read bridge') throw new OpsailChromeError('foreign-registration', 'Refusing to remove a native messaging registration not owned by Opsail.');
      await fs.unlink(registry);
    } catch (error) { if (error.code !== 'ENOENT') throw error; }
  }
  await atomicPrivateJson(path.join(dataRoot, 'runtime', 'disabled.json'), {
    schemaVersion: PROTOCOL_VERSION, disabledAt: new Date().toISOString(), reason: 'uninstalled',
  });
  return { schemaVersion: PROTOCOL_VERSION, operation: 'uninstall', dataRoot, dataRetained: true };
}

export async function doctor(dataRoot = defaultDataRoot, {provider, home, sourceBindingFile = defaultBindingFile} = {}) {
  if (provider !== undefined && !['feishu', 'teams'].includes(provider)) throw new OpsailChromeError('invalid-provider', 'doctor supports feishu or teams.');
  let config, configState = 'available', disabled = false;
  try { config = await loadConfig(dataRoot, {optional: true}); if (!config) configState = 'not-configured'; }
  catch { config = null; configState = 'invalid-config'; }
  try { disabled = !!(await bridgeDisabled(dataRoot)); } catch { disabled = true; }
  const base = provider || configState === 'invalid-config' ? {schemaVersion: PROTOCOL_VERSION, configured: !!config, disabled, dataRoot, extensionId: config?.extensionId ?? null, buildId: config?.buildId ?? null} : await bridgeDoctor(dataRoot);
  const configProviders = config ? Object.fromEntries(Object.entries(config.providers).filter(([name]) => ['feishu', 'teams'].includes(name)).map(([name, value]) => [name, {
    profileId: /^[a-f0-9]{1,32}$/.test(value?.profileId) ? value.profileId : null, pending: value?.pending === true, bound: Boolean(value?.targetUrl && value?.accountHash && value?.tenantHash),
  }])) : {};
  const result = {...base, configState, operation: 'doctor', unpackedPath: path.join(dataRoot, 'runtime/current/extension'), providers: configProviders};
  if (!provider) return result;
  const checks = [];
  const add = (step, state, code, nextStep = null) => checks.push({step, state, code, nextStep});
  const installHint = 'Run opsail-chrome install, then Load unpacked from the reported runtime/current/extension directory.';
  let registered = false;
  try {
    const manifest = await readPrivateJson(chromeNativeManifestPath(home));
    const wrapper = path.join(dataRoot, 'runtime/native-messaging/opsail-native-host');
    const info = await fs.lstat(wrapper);
    registered = !!config && manifest.name === nativeHostName && manifest.type === 'stdio' && manifest.path === wrapper &&
      JSON.stringify(manifest.allowed_origins) === JSON.stringify([`chrome-extension://${config.extensionId}/`]) && info.isFile() && !info.isSymbolicLink() && info.uid === process.getuid() && (info.mode & 0o777) === 0o700;
  } catch {}
  add('native-registration', registered && !base.disabled ? 'pass' : 'fail', base.disabled ? 'bridge-disabled' : registered ? 'registered' : 'registration-unavailable', registered && !base.disabled ? null : installHint);
  const bound = config?.providers?.[provider];
  let online = false;
  try { online = !!bound && await profileOnline(dataRoot, bound.profileId); } catch {}
  add('bound-profile-socket', online ? 'pass' : 'fail', online ? 'online' : 'extension-offline', online ? null : `Start the bound Chrome profile and enable Opsail; if its profile changed, run opsail-chrome bind --provider ${provider} --rebind.`);
  let matched = false, probeCode = 'not-probed';
  if (registered && online && !base.disabled) {
    try {
      const ping = await request('ping', undefined, {}, {dataRoot, profileId: bound.profileId, timeoutMs: 3000});
      matched = ping.profileId === bound.profileId && ping.protocolVersion === PROTOCOL_VERSION && ping.extensionVersion === '0.1.0' && ping.buildId === config.buildId;
      probeCode = matched ? 'build-matched' : 'build-changed';
    } catch (error) { probeCode = ['build-changed', 'implementation-changed', 'timeout', 'extension-offline'].includes(error.code) ? error.code : 'handshake-rejected'; }
  }
  add('handshake-build', matched ? 'pass' : probeCode === 'not-probed' ? 'blocked' : 'fail', probeCode, matched ? null : installHint);
  let probe, probeFailure;
  if (matched) {
    try { probe = await request('diagnose', provider, {}, {dataRoot, profileId: bound.profileId, timeoutMs: 10_000}); }
    catch (error) { probeFailure = error; probeCode = identityNextStep(error.code, provider) ? error.code : 'diagnostic-unavailable'; }
  }
  const valid = isObject(probe) && ['sitePermission', 'paused', 'identityVerified'].every(key => typeof probe[key] === 'boolean');
  const permitted = valid && probe.sitePermission;
  add('site-permission', !valid ? 'blocked' : permitted ? 'pass' : 'fail', !valid ? probeCode : permitted ? 'granted' : 'site-permission-required', permitted ? null : !valid ? 'Resolve the preceding registration/socket/handshake blockers and retry doctor.' : 'Open Opsail extension options, grant the configured site, then retry this doctor command.');
  const identityCodes = ['identity-verified', 'identity-unverified', 'unqualified-build', 'provider-degraded', 'target-tab-conflict', 'identity-read-failed', 'login-required', 'account-changed', 'tenant-changed', 'page-changed', 'document-changed', 'binding-changed', 'tab-missing', 'tab-not-prepared', 'not-bound', 'not-probed'];
  const identityCode = valid && identityCodes.includes(probe.identityCode) ? probe.identityCode : identityCodes.includes(probeFailure?.code) ? probeFailure.code : 'not-probed';
  const identity = permitted && probe.identityVerified && identityCode === 'identity-verified';
  add('identity', identity ? 'pass' : identityCode === 'not-probed' ? 'blocked' : 'fail', identityCode,
    identity ? null : identityNextStep(identityCode, provider) || (['tab-not-prepared', 'tab-missing', 'document-changed', 'binding-changed'].includes(identityCode) ? `Run chat prepare --provider ${provider}, then retry this doctor command.` : 'Sign in to the originally bound account and tenant; resolve pause/permission blockers, then retry doctor.'));
  const paused = valid && probe.paused;
  add('pause', !valid ? 'blocked' : paused ? 'fail' : 'pass', !valid ? 'not-probed' : paused ? 'paused' : 'not-paused', paused ? 'Use Opsail extension options to resume reads, then retry doctor.' : !valid ? 'Resolve the preceding probe blockers and retry doctor.' : null);
  let consistent = false;
  try {
    const source = await readPrivateJson(sourceBindingFile, {optional: true});
    const browser = source?.providers?.[provider]?.browser;
    consistent = !!bound && bound.pending !== true && /^[a-f0-9]{64}$/.test(bound.accountHash) && /^[a-f0-9]{64}$/.test(bound.tenantHash) &&
      (bound.authoritative === true || (browser?.transport === 'chrome-extension' && browser.profile_id === bound.profileId && browser.account_hash === bound.accountHash && browser.tenant_hash === bound.tenantHash));
  } catch {}
  add('binding-state', consistent ? 'pass' : 'fail', bound?.pending ? 'binding-pending' : consistent ? 'consistent' : 'binding-inconsistent', consistent ? null : `Run opsail-chrome bind --provider ${provider} --profile-id PROFILE_ID after confirming the original account; use --rebind for an offline old profile.`);
  const inventory = await runtimeProfiles(dataRoot);
  if (!online && bound && inventory.profiles.length === 1) checks.find(item => item.step === 'bound-profile-socket').nextStep = `Exactly one other profile is online. Run opsail-chrome bind --provider ${provider} --rebind; it must verify both stored identity hashes before switching.`;
  // Same rule as prune-runtime: a bound profile is never stale. Its offline
  // state belongs to its own provider's bound-profile-socket check.
  const boundProfiles = new Set(Object.values(configProviders).map(value => value.profileId).filter(Boolean));
  const stale = inventory.stale.filter(id => !boundProfiles.has(id)), offlineBound = inventory.stale.filter(id => boundProfiles.has(id));
  add('runtime-records', stale.length ? 'fail' : 'pass', stale.length ? 'stale-runtime-records' : 'current', stale.length ? 'Run opsail-chrome prune-runtime, then retry doctor. Cleanup preserves every bound, live or uncertain profile.' : null);
  return {...result, profiles: inventory.profiles, staleProfiles: stale, staleRuntimeRecords: stale.length, offlineBoundProfiles: offlineBound, provider, diagnostics: boundedDiagnostics(probe?.diagnostics ?? probeFailure?.diagnostics), ready: checks.every(item => item.state === 'pass'), checks};
}

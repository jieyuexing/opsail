import {promises as fs} from 'node:fs';
import path from 'node:path';
import {defaultDataRoot, ensurePrivateDir, loadConfig, readPrivateJson, PROFILE_ID, runtimeSocketPath, OpsailChromeError} from './native/common.mjs';
import {runtimeSocketState} from './client.mjs';

export async function pruneRuntime(dataRoot = defaultDataRoot) {
  // Establish config authority before creating even a lock. Never treat a
  // missing/corrupt config as an empty set of bindings.
  await loadConfig(dataRoot);
  const runtime = path.join(dataRoot, 'runtime');
  await ensurePrivateDir(runtime);
  const lock = path.join(runtime, 'config.lock');
  try { await fs.mkdir(lock, {mode: 0o700}); }
  catch (error) { if (error.code === 'EEXIST') throw new OpsailChromeError('binding-busy', 'Another configuration or runtime operation is in progress.'); throw error; }
  try {
    const config = await loadConfig(dataRoot), original = JSON.stringify(config);
    const bound = new Set();
    for (const provider of Object.values(config.providers)) {
      if (!provider || typeof provider.profileId !== 'string' || !PROFILE_ID.test(provider.profileId)) throw new OpsailChromeError('invalid-config', 'Cannot establish all bound profiles; no runtime records were pruned.');
      bound.add(provider.profileId);
    }
    const entries = (await fs.readdir(runtime, {withFileTypes: true})).filter(e => /^[a-f0-9]{1,32}\.json$/.test(e.name)).sort((a, b) => a.name.localeCompare(b.name));
    if (entries.length > 128) throw new OpsailChromeError('runtime-record-limit', 'Runtime profile inventory exceeds 128 records; no records were pruned.');
    const removed = [], kept = [];
    const receipt = (completed = true, reason) => ({operation: 'prune-runtime', completed, ...(reason ? {reason} : {}), removedCount: removed.length, removed, kept});
    for (const entry of entries) {
      const profileId = entry.name.slice(0, -5), file = path.join(runtime, entry.name);
      const keep = reason => kept.push({profileId, reason});
      if (bound.has(profileId)) { keep('bound'); continue; }
      let record, before;
      try { before = await fs.lstat(file); record = await readPrivateJson(file); }
      catch { keep('unsafe-record'); continue; }
      if (record?.schemaVersion !== 1 || record.profileId !== profileId || record.socket !== undefined && record.socket !== path.basename(runtimeSocketPath(dataRoot, profileId))) { keep('invalid-record'); continue; }
      const state = await runtimeSocketState(dataRoot, profileId);
      if (state !== 'offline') { keep(state); continue; }
      // The config lock also covers native startup through socket.listen().
      // Refuse out-of-band config writes or replaced/edited metadata.
      const finalState = await runtimeSocketState(dataRoot, profileId);
      if (finalState !== 'offline') { keep(finalState); continue; }
      try {
        if (JSON.stringify(await loadConfig(dataRoot)) !== original) return receipt(false, 'binding-changed');
      } catch { return receipt(false, 'binding-changed'); }
      let after;
      try { after = await fs.lstat(file); } catch { keep('record-changed'); continue; }
      if (!after.isFile() || after.dev !== before.dev || after.ino !== before.ino || after.size !== before.size || after.mtimeMs !== before.mtimeMs || after.ctimeMs !== before.ctimeMs) { keep('record-changed'); continue; }
      try { await fs.unlink(file); } catch { keep('remove-failed'); return receipt(false, 'remove-failed'); }
      removed.push({profileId, record: `runtime/${entry.name}`});
    }
    return receipt();
  } finally { await fs.rmdir(lock); }
}

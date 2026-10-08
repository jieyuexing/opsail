import {boundedDiagnostics, identityNextStep} from './extension/diagnostics.js';
import { defaultDataRoot, OpsailChromeError } from './native/common.mjs';
import { bind, doctor, install, qualify, uninstall } from './install.mjs';
import {pruneRuntime} from './prune-runtime.mjs';

function fail(error) {
  process.stderr.write(`${JSON.stringify({ schemaVersion: 1, ok: false, error: { code: error.code || 'internal-error', message: error.message || 'Opsail Chrome command failed.', ...(error.diagnostics ? {diagnostics: boundedDiagnostics(error.diagnostics)} : {}), ...(identityNextStep(error.code) ? {nextStep: identityNextStep(error.code)} : {}) } })}\n`);
  process.exitCode = 2;
}
function success(value, quiet) { if (!quiet) process.stdout.write(`${JSON.stringify({ schemaVersion: 1, ok: true, ...value })}\n`); }
function options(values) {
  const output = { positional: [] };
  for (let i = 0; i < values.length; i += 1) {
    const value = values[i];
    if (!value.startsWith('--')) output.positional.push(value);
    else if (['--quiet', '--rebind', '--by-operator'].includes(value)) output[value.slice(2).replace(/-([a-z])/g, (_, c) => c.toUpperCase())] = true;
    else if (['--data-root', '--profile-id', '--provider', '--build', '--extension-source', '--home'].includes(value)) {
      const key = value.slice(2).replace(/-([a-z])/g, (_, c) => c.toUpperCase()); const next = values[++i];
      if (!next || next.startsWith('--') || output[key] !== undefined) throw new OpsailChromeError('invalid-arguments', `Argument ${value} requires one value and may be supplied once.`);
      output[key] = next;
    }
    else throw new OpsailChromeError('invalid-arguments', `Unsupported argument: ${value}`);
  }
  return output;
}
async function main() {
  const parsed = options(process.argv.slice(2)); const [command] = parsed.positional;
  if (parsed.positional.length !== 1) throw new OpsailChromeError('invalid-arguments', 'Specify exactly one command.');
  if (parsed.rebind && command !== 'bind') throw new OpsailChromeError('invalid-arguments', '--rebind is only valid with bind.');
  if ((parsed.build !== undefined || parsed.byOperator) && command !== 'qualify') throw new OpsailChromeError('invalid-arguments', '--build and --by-operator are only valid with qualify.');
  const dataRoot = parsed.dataRoot || defaultDataRoot;
  let result;
  if (command === 'install') result = await install({ dataRoot, extensionSource: parsed.extensionSource, home: parsed.home });
  else if (command === 'doctor') result = await doctor(dataRoot, {provider: parsed.provider, home: parsed.home});
  else if (command === 'prune-runtime') {
    if (parsed.provider || parsed.profileId) throw new OpsailChromeError('invalid-arguments', 'prune-runtime checks all profiles; do not pass a provider or profile ID.');
    result = await pruneRuntime(dataRoot);
  }
  else if (command === 'bind') result = await bind(parsed.provider, { profileId: parsed.profileId, rebind: parsed.rebind === true, dataRoot });
  else if (command === 'qualify') result = await qualify(parsed.provider, {build: parsed.build, byOperator: parsed.byOperator === true, dataRoot});
  else if (command === 'uninstall') result = await uninstall({ dataRoot, home: parsed.home });
  else throw new OpsailChromeError('invalid-command', 'Use install, doctor, prune-runtime, bind, qualify, or uninstall.');
  success(result, parsed.quiet);
  if (command === 'doctor' && result.ready === false) process.exitCode = 2;
  if (command === 'prune-runtime' && result.completed === false) process.exitCode = 2;
}
main().catch(fail);

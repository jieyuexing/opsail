import assert from 'node:assert/strict';
import {mkdtemp, readFile, chmod, rm, mkdir, writeFile, copyFile} from 'node:fs/promises';
import {tmpdir} from '../temp-root.mjs';
import path from 'node:path';
import {createServer} from 'node:net';
import test from 'node:test';
import {execFile} from 'node:child_process';
import {promisify} from 'node:util';
import {bind, qualify} from '../../../src/browser/install.mjs';
import {atomicPrivateJson, assertAllowedRequest, runtimeSocketPath} from '../../../src/browser/native/common.mjs';

async function setup(t) {
  const root=await mkdtemp(path.join(tmpdir(),'r-'));t.after(()=>rm(root,{recursive:true,force:true}));
  const sourceBindingFile=path.join(root,'bindings.json');
  const bound={profileId:'aa',pending:false,targetUrl:'https://x.feishu.cn/next/',allowedOrigins:['https://x.feishu.cn'],accountHash:'a'.repeat(64),tenantHash:'b'.repeat(64)};
  const config={schemaVersion:1,dataRoot:root,extensionId:'a'.repeat(32),buildId:'c'.repeat(64),nodePath:process.execPath,providers:{feishu:bound,teams:{untouched:true}}};
  await atomicPrivateJson(path.join(root,'config.json'),config);
  await atomicPrivateJson(sourceBindingFile,{schemaVersion:1,providers:{feishu:{snapshot_root:'/fixture/snapshots',browser:{transport:'chrome-extension',target_url:bound.targetUrl,allowed_origins:bound.allowedOrigins,profile_id:'aa',account_hash:bound.accountHash,tenant_hash:bound.tenantHash}},wechat:{snapshot_root:'/fixture/wechat'}}});
  const initialConfig=await readFile(path.join(root,'config.json'),'utf8'), initialSource=await readFile(sourceBindingFile,'utf8');
  const calls=[];const behavior={error:null,accountHash:bound.accountHash,tenantHash:bound.tenantHash,ready:true,after:null};
  async function online(profileId) {
    await mkdir(path.join(root,'runtime'),{recursive:true});
    const sockets=new Set();const server=createServer(socket=>{sockets.add(socket);socket.on('close',()=>sockets.delete(socket));let buffer='';socket.on('data',async chunk=>{buffer+=chunk;if(!buffer.includes('\n'))return;
      const request=JSON.parse(buffer);calls.push(request);assertAllowedRequest(request);
      if(behavior.after)await behavior.after();
      socket.end(JSON.stringify({type:'response',protocolVersion:1,requestId:request.requestId,ok:!behavior.error,...(behavior.error?{error:{code:behavior.error,message:'PRIVATE_PROVIDER_TEXT',diagnostics:behavior.diagnostics}}:{data:{ready:behavior.ready,profileId,accountHash:behavior.accountHash,tenantHash:behavior.tenantHash,diagnostics:behavior.diagnostics}})})+'\n');
    });});
    await new Promise(resolve=>server.listen(runtimeSocketPath(root,profileId),resolve));await chmod(runtimeSocketPath(root,profileId),0o600);
    await atomicPrivateJson(path.join(root,`runtime/${profileId}.json`),{schemaVersion:1,profileId});
    t.after(async()=>{for(const socket of sockets)socket.destroy();await new Promise(resolve=>server.close(resolve));});
  }
  return {root,dataRoot:root,sourceBindingFile,bound,config,initialConfig,initialSource,calls,behavior,online};
}
async function unchanged(f) {
  assert.equal(await readFile(path.join(f.root,'config.json'),'utf8'),f.initialConfig);
  assert.equal(await readFile(f.sourceBindingFile,'utf8'),f.initialSource);
}

test('rebind verifies both hashes and atomically switches one authority, preserving legacy snapshots',async t=>{
  const f=await setup(t);await f.online('bb');
  const result=await bind('feishu',{...f,rebind:true});
  assert.equal(result.profileId,'bb');assert.equal(result.identityVerified,true);
  assert.doesNotMatch(JSON.stringify(result),new RegExp(f.bound.accountHash+'|'+f.bound.tenantHash));
  assert.equal(f.calls.length,1);assert.equal(f.calls[0].operation,'verifyBinding');
  assert.equal(f.calls[0].binding.accountHash,f.bound.accountHash);assert.equal(f.calls[0].binding.tenantHash,f.bound.tenantHash);
  const saved=JSON.parse(await readFile(path.join(f.root,'config.json'),'utf8'));
  assert.equal(saved.providers.feishu.authoritative,true);assert.equal(saved.providers.feishu.pending,false);assert.equal(saved.providers.feishu.profileId,'bb');
  assert.deepEqual(saved.providers.teams,f.config.providers.teams);
  assert.equal(await readFile(f.sourceBindingFile,'utf8'),f.initialSource);
});

test('rebind refuses each hash mismatch and every failed probe without pending state',async t=>{
  const f=await setup(t);await f.online('bb');
  for(const failure of [{accountHash:'c'.repeat(64)},{tenantHash:'c'.repeat(64)},{ready:false},{error:'site-permission-required'},{error:'paused'},{error:'PRIVATE_CODE'}]) {
    Object.assign(f.behavior,{accountHash:f.bound.accountHash,tenantHash:f.bound.tenantHash,ready:true,error:null},failure);
    await assert.rejects(bind('feishu',{...f,rebind:true}),error=>{assert.doesNotMatch(error.message,/PRIVATE/);return true;});
    await unchanged(f);
  }
});

test('rebind refuses an online old profile, multiple replacements and no replacements',async t=>{
  const f=await setup(t);
  await assert.rejects(bind('feishu',{...f,rebind:true}),{code:'extension-offline'});
  await f.online('bb');await f.online('cc');
  await assert.rejects(bind('feishu',{...f,rebind:true}),{code:'ambiguous-profiles'});
  await f.online('aa');
  await assert.rejects(bind('feishu',{...f,rebind:true}),{code:'bound-profile-online'});
  assert.equal(f.calls.length,0);await unchanged(f);
});

test('profile changes during verification and concurrent configuration writers fail closed',async t=>{
  const f=await setup(t);await f.online('bb');f.behavior.after=async()=>{f.behavior.after=null;await f.online('cc');};
  await assert.rejects(bind('feishu',{...f,rebind:true}),{code:'profiles-changed'});await unchanged(f);
  await mkdir(path.join(f.root,'runtime/config.lock'));
  await assert.rejects(bind('feishu',{...f,profileId:'bb'}),{code:'binding-busy'});await unchanged(f);
});

test('normal bind also verifies before its only commit; arbitrary binding injection is rejected',async t=>{
  const f=await setup(t);await f.online('bb');f.behavior.error='identity-unverified';
  await assert.rejects(bind('feishu',{...f,profileId:'bb'}),{code:'identity-unverified'});await unchanged(f);
  assert.throws(()=>assertAllowedRequest({protocolVersion:1,requestId:'r',operation:'read',provider:'feishu',args:{},binding:{targetUrl:f.bound.targetUrl,allowedOrigins:f.bound.allowedOrigins}}),{code:'invalid-request'});
  assert.equal(assertAllowedRequest({protocolVersion:1,requestId:'r',operation:'scrollBack',provider:'teams',args:{conversationId:'c1',direction:'up'}}).operation,'scrollBack');
  assert.throws(()=>assertAllowedRequest({protocolVersion:1,requestId:'r',operation:'scrollBack',provider:'teams',args:{conversationId:'c1',direction:'down'}}),{code:'invalid-request'});
  assert.throws(()=>assertAllowedRequest({protocolVersion:1,requestId:'r',operation:'scrollBack',args:{conversationId:'c1',direction:'up'}}),{code:'provider-required'});
  f.behavior.error=null;await bind('feishu',{...f,profileId:'bb'});
  assert.equal(await readFile(f.sourceBindingFile,'utf8'),f.initialSource);
});


test('Teams rebind uses the stored authority and detects a concurrent source edit',async t=>{
  const f=await setup(t);await f.online('bb');
  f.config.providers={teams:{...f.bound,targetUrl:'https://teams.microsoft.com/v2/',allowedOrigins:['https://teams.microsoft.com'],authoritative:true}};
  await atomicPrivateJson(path.join(f.root,'config.json'),f.config);
  const saved=await readFile(path.join(f.root,'config.json'),'utf8');
  f.behavior.after=async()=>{await atomicPrivateJson(f.sourceBindingFile,{schemaVersion:1,providers:{wechat:{snapshot_root:'/fixture/concurrent'}}});};
  await assert.rejects(bind('teams',{...f,rebind:true}),{code:'binding-changed'});
  assert.equal(await readFile(path.join(f.root,'config.json'),'utf8'),saved);
  f.behavior.after=null;
  const result=await bind('teams',{...f,rebind:true});
  assert.equal(result.provider,'teams');assert.equal(result.profileId,'bb');
  assert.equal(f.calls.at(-1).binding.targetUrl,'https://teams.microsoft.com/v2/');
});

async function cli(f, args) {
  // Isolate even the CLI's default legacy binding lookup from the real root.
  const source = path.resolve(f.root, 'bindings.json');
  await mkdir(path.dirname(source), {recursive: true}); await copyFile(f.sourceBindingFile, source); await chmod(source, 0o600);
  try {
    const result = await promisify(execFile)(process.execPath, [new URL('../../../src/browser/opsail-chrome.mjs', import.meta.url).pathname, ...args, '--data-root', f.root], {env: {...process.env, OPSAIL_CHAT_BINDING_FILE: source}});
    return {code: 0, ...result};
  } catch (error) { return {code: error.code, stdout: error.stdout, stderr: error.stderr}; }
}

test('qualify CLI adds exactly one reviewed name, keeps the default and records operator-only provenance', async t => {
  const f = await setup(t);
  const command = ['qualify', '--provider', 'feishu', '--build', 'index.abcdef01.js', '--by-operator'];
  const first = await cli(f, command); assert.equal(first.code, 0, first.stderr);
  const output = JSON.parse(first.stdout); assert.equal(output.added, true); assert.equal(output.qualification, 'qualified-by-operator');
  assert.equal(output.behavioralContractObserved, false); assert.match(output.note, /NOT observed/);
  const saved = JSON.parse(await readFile(path.join(f.root, 'config.json'), 'utf8'));
  assert.deepEqual(saved.identityBuilds.feishu, [{name: 'index.745e4057.js', qualification: 'built-in'}, {name: 'index.abcdef01.js', qualification: 'qualified-by-operator'}]);
  assert.deepEqual(saved.providers, f.config.providers);
  const before = await readFile(path.join(f.root, 'config.json'), 'utf8');
  assert.equal(JSON.parse((await cli(f, command)).stdout).added, false);
  assert.equal(await readFile(path.join(f.root, 'config.json'), 'utf8'), before);
  for (const name of ['index.*.js', '../index.abcdef01.js', 'https://x/index.abcdef01.js', 'index.abcdef01.js?token=x', 'index.ABCDEF01.js', 'index.abc.js', 'index.abcdef01.js\n']) {
    const rejected = await cli(f, ['qualify', '--provider', 'feishu', '--build', name, '--by-operator']);
    assert.equal(rejected.code, 2); assert.equal(JSON.parse(rejected.stderr).error.code, 'invalid-build');
    assert.equal(await readFile(path.join(f.root, 'config.json'), 'utf8'), before);
  }
  await assert.rejects(qualify('teams', {dataRoot: f.root, build: 'index.abcdef02.js', byOperator: true}), {code: 'invalid-provider'});
  assert.equal(f.calls.length, 0);
});

test('qualify requires the bound profile, observed contract, exact bundle and both hashes before persistence', async t => {
  const f = await setup(t), options = {dataRoot: f.root, build: 'index.abcdef01.js'};
  await f.online('bb');
  await assert.rejects(qualify('feishu', options), {code: 'bound-profile-offline'}); await unchanged(f);
  assert.equal(f.calls.length, 0);
  await f.online('aa');
  for (const change of [
    {diagnostics: {bundleNames: ['index.abcdef01.js'], behavioralContractPassed: false}},
    {diagnostics: {bundleNames: ['index.abcdef02.js'], behavioralContractPassed: true}},
    {accountHash: 'c'.repeat(64)}, {tenantHash: 'c'.repeat(64)}, {error: 'unqualified-build'},
  ]) {
    Object.assign(f.behavior, {accountHash: f.bound.accountHash, tenantHash: f.bound.tenantHash, error: null, diagnostics: {bundleNames: ['index.abcdef01.js'], behavioralContractPassed: true}}, change);
    await assert.rejects(qualify('feishu', options)); await unchanged(f);
  }
  Object.assign(f.behavior, {accountHash: f.bound.accountHash, tenantHash: f.bound.tenantHash, error: null, diagnostics: {bundleNames: ['index.abcdef01.js'], behavioralContractPassed: true}});
  const result = await qualify('feishu', options);
  assert.equal(result.qualification, 'behavior-verified'); assert.equal(result.behavioralContractObserved, true);
  assert.equal(f.calls.at(-1).operation, 'verifyBuild'); assert.deepEqual(f.calls.at(-1).args, {build: options.build});
  assert.equal(f.calls.at(-1).binding, undefined);
});

test('qualification respects config lock, concurrent edits and malformed allow-lists', async t => {
  const f = await setup(t), options = {dataRoot: f.root, build: 'index.abcdef01.js'};
  await f.online('aa');
  await mkdir(path.join(f.root, 'runtime/config.lock'));
  await assert.rejects(qualify('feishu', {...options, byOperator: true}), {code: 'binding-busy'}); await unchanged(f);
  await rm(path.join(f.root, 'runtime/config.lock'), {recursive: true});
  f.behavior.diagnostics = {bundleNames: [options.build], behavioralContractPassed: true};
  f.behavior.after = async () => atomicPrivateJson(path.join(f.root, 'config.json'), {...f.config, concurrent: true});
  await assert.rejects(qualify('feishu', options), {code: 'binding-changed'});
  assert.equal(JSON.parse(await readFile(path.join(f.root, 'config.json'), 'utf8')).identityBuilds, undefined);
  await atomicPrivateJson(path.join(f.root, 'config.json'), {...f.config, identityBuilds: {feishu: [{name: '*', qualification: 'qualified-by-operator'}]}});
  await assert.rejects(qualify('feishu', {...options, byOperator: true}), {code: 'invalid-config'});
});

test('bind and rebind CLI preserve bounded failure diagnostics while discarding private provider text', async t => {
  const f = await setup(t); await f.online('bb');
  f.behavior.error = 'unqualified-build';
  f.behavior.diagnostics = {bundleNames: ['index.abcdef01.js', 'https://PRIVATE/'], buildQualified: false, accountId: 'PRIVATE', bootstrap: {globals: [{name: 'PRIVATE', fields: [{key: 'PRIVATE', type: 'string'}]}]}};
  for (const flags of [['--profile-id', 'bb'], ['--rebind']]) {
    const result = await cli(f, ['bind', '--provider', 'feishu', ...flags]);
    assert.equal(result.code, 2); const error = JSON.parse(result.stderr).error;
    assert.equal(error.code, 'unqualified-build'); assert.match(error.nextStep, /qualify/);
    assert.deepEqual(error.diagnostics.bundleNames, ['index.abcdef01.js']); assert.equal(error.diagnostics.bootstrap.fieldTypes.string, 1);
    assert.doesNotMatch(result.stderr, /PRIVATE/); await unchanged(f);
  }
  for (const code of ['provider-degraded', 'target-tab-conflict', 'identity-read-failed']) {
    f.behavior.error = code;
    const result = await cli(f, ['bind', '--provider', 'feishu', '--rebind']);
    assert.equal(JSON.parse(result.stderr).error.code, code); assert.equal(result.code, 2); await unchanged(f);
    if (code !== 'identity-read-failed') assert.match(JSON.parse(result.stderr).error.nextStep, /Close other Feishu messenger tabs/);
  }
});

test('Teams rebind CLI verifies both hashes and preserves all authority on each failed probe', async t => {
  const f = await setup(t); await f.online('bb');
  f.config.providers.teams = {...f.bound, authoritative: true, targetUrl: 'https://teams.microsoft.com/v2/', allowedOrigins: ['https://teams.microsoft.com']};
  await atomicPrivateJson(path.join(f.root, 'config.json'), f.config);
  f.initialConfig = await readFile(path.join(f.root, 'config.json'), 'utf8');
  for (const failure of [{accountHash: 'c'.repeat(64)}, {tenantHash: 'c'.repeat(64)}, {ready: false}, {error: 'site-permission-required'}, {error: 'identity-unverified'}, {error: 'paused'}]) {
    Object.assign(f.behavior, {accountHash: f.bound.accountHash, tenantHash: f.bound.tenantHash, ready: true, error: null}, failure);
    const result = await cli(f, ['bind', '--provider', 'teams', '--rebind']);
    assert.equal(result.code, 2); await unchanged(f);
    const probe = f.calls.at(-1);
    assert.equal(probe.operation, 'verifyBinding'); assert.equal(probe.provider, 'teams');
    assert.equal(probe.binding.accountHash, f.bound.accountHash); assert.equal(probe.binding.tenantHash, f.bound.tenantHash);
  }
  Object.assign(f.behavior, {accountHash: f.bound.accountHash, tenantHash: f.bound.tenantHash, ready: true, error: null});
  const success = await cli(f, ['bind', '--provider', 'teams', '--rebind']);
  assert.equal(success.code, 0, success.stderr); assert.equal(JSON.parse(success.stdout).identityVerified, true);
  const saved = JSON.parse(await readFile(path.join(f.root, 'config.json'), 'utf8'));
  assert.equal(saved.providers.teams.profileId, 'bb'); assert.equal(saved.providers.teams.authoritative, true);
  assert.deepEqual(saved.providers.feishu, f.config.providers.feishu);
  assert.equal(await readFile(f.sourceBindingFile, 'utf8'), f.initialSource);
});

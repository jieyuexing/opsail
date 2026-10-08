import assert from 'node:assert/strict';
import {mkdtemp, mkdir, chmod, writeFile, readFile, readdir, rm} from 'node:fs/promises';
import {tmpdir} from '../temp-root.mjs';
import path from 'node:path';
import {createServer} from 'node:net';
import test from 'node:test';
import {doctor, chromeNativeManifestPath} from '../../../src/browser/install.mjs';
import {atomicPrivateJson, runtimeSocketPath} from '../../../src/browser/native/common.mjs';

export async function fixture(t) {
  const root=await mkdtemp(path.join(tmpdir(),'d-')); t.after(()=>rm(root,{recursive:true,force:true}));
  const home=path.join(root,'home'), sourceBindingFile=path.join(root,'bindings.json');
  const bound={profileId:'ab',pending:false,targetUrl:'https://x.feishu.cn/next/',allowedOrigins:['https://x.feishu.cn'],accountHash:'a'.repeat(64),tenantHash:'b'.repeat(64)};
  const config={schemaVersion:1,dataRoot:root,extensionId:'a'.repeat(32),buildId:'c'.repeat(64),nodePath:process.execPath,providers:{feishu:bound}};
  await atomicPrivateJson(path.join(root,'config.json'),config);
  await atomicPrivateJson(sourceBindingFile,{schemaVersion:1,providers:{feishu:{snapshot_root:'/fixture/snapshots',browser:{transport:'chrome-extension',target_url:bound.targetUrl,allowed_origins:bound.allowedOrigins,profile_id:bound.profileId,account_hash:bound.accountHash,tenant_hash:bound.tenantHash}}}});
  const wrapper=path.join(root,'runtime/native-messaging/opsail-native-host'); await mkdir(path.dirname(wrapper),{recursive:true}); await writeFile(wrapper,'#!/bin/sh\n',{mode:0o700});
  await atomicPrivateJson(chromeNativeManifestPath(home),{name:'com.opsail.chrome',type:'stdio',path:wrapper,allowed_origins:[`chrome-extension://${config.extensionId}/`]});
  const calls=[];
  const state={ping:{profileId:'ab',protocolVersion:1,extensionVersion:'0.1.0',buildId:config.buildId},probe:{sitePermission:true,paused:false,identityVerified:true,identityCode:'identity-verified'}};
  const sockets=new Set();
  const server=createServer(socket=>{sockets.add(socket);socket.on('close',()=>sockets.delete(socket));let buffer='';socket.on('data',chunk=>{buffer+=chunk;if(!buffer.includes('\n'))return;const r=JSON.parse(buffer);calls.push(r.operation);socket.end(JSON.stringify({type:'response',protocolVersion:1,requestId:r.requestId,ok:!(r.operation==='diagnose'&&state.error),...(r.operation==='diagnose'&&state.error?{error:{code:state.error,message:'PRIVATE',diagnostics:state.probe.diagnostics}}:{data:r.operation==='ping'?state.ping:state.probe})})+'\n');});});
  await new Promise(resolve=>server.listen(runtimeSocketPath(root,'ab'),resolve));await chmod(runtimeSocketPath(root,'ab'),0o600);
  t.after(async()=>{for(const socket of sockets)socket.destroy();await new Promise(resolve=>server.close(resolve));});
  await atomicPrivateJson(path.join(root,'runtime/ab.json'),{schemaVersion:1,profileId:'ab'});
  return {root,home,sourceBindingFile,config,bound,calls,state};
}

test('provider doctor is ordered, metadata only, and makes only ping/diagnose requests',async t=>{
  const f=await fixture(t);
  const before=await readFile(path.join(f.root,'config.json'),'utf8');
  const result=await doctor(f.root,f);
  // No provider means inventory only.
  assert.equal(result.ready,undefined);assert.deepEqual(f.calls,[]);
  const ready=await doctor(f.root,{...f,provider:'feishu'});
  assert.equal(ready.ready,true);assert.deepEqual(f.calls,['ping','diagnose']);
  assert.deepEqual(ready.checks.map(c=>c.step),['native-registration','bound-profile-socket','handshake-build','site-permission','identity','pause','binding-state','runtime-records']);
  assert.equal(await readFile(path.join(f.root,'config.json'),'utf8'),before);
  assert.doesNotMatch(JSON.stringify(ready),new RegExp(f.bound.accountHash+'|'+f.bound.tenantHash));
});

test('doctor separates permission, identity, pause, pending and stale records with hints',async t=>{
  const f=await fixture(t);
  for(const probe of [
    {sitePermission:false,paused:false,identityVerified:false,identityCode:'not-probed'},
    {sitePermission:true,paused:false,identityVerified:false,identityCode:'tab-not-prepared'},
    {sitePermission:true,paused:true,identityVerified:false,identityCode:'not-probed'},
    {sitePermission:true,paused:false,identityVerified:false,identityCode:'PRIVATE_PROVIDER_TEXT'},
  ]) {
    f.state.probe=probe;const r=await doctor(f.root,{...f,provider:'feishu'});
    assert.equal(r.ready,false);assert.ok(r.checks.filter(c=>c.state!=='pass').every(c=>c.nextStep));
    assert.doesNotMatch(JSON.stringify(r),/PRIVATE_PROVIDER_TEXT/);
  }
  f.config.providers.feishu.pending=true;await atomicPrivateJson(path.join(f.root,'config.json'),f.config);
  await atomicPrivateJson(path.join(f.root,'runtime/cd.json'),{schemaVersion:1,profileId:'cd'});
  const stale=await doctor(f.root,{...f,provider:'feishu'});
  assert.equal(stale.staleRuntimeRecords,1);assert.ok(stale.checks.some(c=>c.code==='binding-pending'));
  assert.ok((await readdir(path.join(f.root,'runtime'))).includes('cd.json'));
});

test('doctor matches prune-runtime: an offline profile bound by another provider is not stale',async t=>{
  const f=await fixture(t);
  f.config.providers.teams={...f.bound,profileId:'ef',targetUrl:'https://teams.microsoft.com/v2/',allowedOrigins:['https://teams.microsoft.com'],authoritative:true};
  await atomicPrivateJson(path.join(f.root,'config.json'),f.config);
  await atomicPrivateJson(path.join(f.root,'runtime/ef.json'),{schemaVersion:1,profileId:'ef'});
  const feishu=await doctor(f.root,{...f,provider:'feishu'});
  assert.equal(feishu.ready,true);assert.equal(feishu.staleRuntimeRecords,0);assert.deepEqual(feishu.offlineBoundProfiles,['ef']);
  const teams=await doctor(f.root,{...f,provider:'teams'});
  assert.equal(teams.checks.find(c=>c.step==='bound-profile-socket').code,'extension-offline');
  assert.equal(teams.checks.find(c=>c.step==='runtime-records').state,'pass');
  await atomicPrivateJson(path.join(f.root,'runtime/cd.json'),{schemaVersion:1,profileId:'cd'});
  const stale=await doctor(f.root,{...f,provider:'feishu'});
  assert.deepEqual(stale.staleProfiles,['cd']);assert.equal(stale.checks.find(c=>c.step==='runtime-records').code,'stale-runtime-records');
});

test('doctor does not call site probes when registration or handshake is invalid',async t=>{
  const f=await fixture(t);f.state.ping.buildId='wrong';
  const mismatch=await doctor(f.root,{...f,provider:'feishu'});
  assert.deepEqual(f.calls,['ping']);assert.equal(mismatch.checks[2].code,'build-changed');
  await rm(chromeNativeManifestPath(f.home));f.calls.length=0;
  const unregistered=await doctor(f.root,{...f,provider:'feishu'});
  assert.deepEqual(f.calls,[]);assert.equal(unregistered.checks[0].code,'registration-unavailable');
});


test('Teams doctor follows the same ordered protocol and treats authoritative bindings as consistent',async t=>{
  const f=await fixture(t);f.config.providers={teams:{...f.bound,targetUrl:'https://teams.microsoft.com/v2/',allowedOrigins:['https://teams.microsoft.com'],authoritative:true}};
  await atomicPrivateJson(path.join(f.root,'config.json'),f.config);
  const result=await doctor(f.root,{...f,provider:'teams'});
  assert.equal(result.ready,true);assert.deepEqual(f.calls,['ping','diagnose']);
});


test('damaged installation still yields ordered bounded blockers without parser excerpts',async t=>{
  const f=await fixture(t);await writeFile(path.join(f.root,'config.json'),'PRIVATE_PROVIDER_TEXT invalid json');
  const result=await doctor(f.root,{...f,provider:'feishu'});
  assert.equal(result.ready,false);assert.equal(result.configState,'invalid-config');
  assert.equal(result.checks.length,8);assert.deepEqual(f.calls,[]);
  assert.doesNotMatch(JSON.stringify(result),/PRIVATE_PROVIDER_TEXT/);
});

test('doctor carries bounded build/tab diagnostics with actionable codes and never private text', async t => {
  const f = await fixture(t);
  for (const identityCode of ['unqualified-build', 'provider-degraded', 'target-tab-conflict', 'identity-read-failed']) {
    f.state.probe = {sitePermission: true, paused: false, identityVerified: false, identityCode,
      diagnostics: {bundleNames: ['index.abcdef01.js', 'https://PRIVATE/'], providerDegraded: identityCode === 'provider-degraded', conflictingTabCount: 1, accountId: 'PRIVATE'}};
    const result = await doctor(f.root, {...f, provider: 'feishu'});
    assert.equal(result.checks[4].code, identityCode); assert.equal(result.ready, false);
    assert.deepEqual(result.diagnostics.bundleNames, ['index.abcdef01.js']); assert.doesNotMatch(JSON.stringify(result), /PRIVATE/);
    if (identityCode === 'unqualified-build') assert.match(result.checks[4].nextStep, /qualify/);
    if (['provider-degraded', 'target-tab-conflict'].includes(identityCode)) assert.match(result.checks[4].nextStep, /Close other Feishu messenger tabs/);
  }
  f.state.error = 'provider-degraded';
  const failedProbe = await doctor(f.root, {...f, provider: 'feishu'});
  assert.equal(failedProbe.checks[4].code, 'provider-degraded');
  assert.deepEqual(failedProbe.diagnostics.bundleNames, ['index.abcdef01.js']);
  assert.doesNotMatch(JSON.stringify(failedProbe), /PRIVATE/);
});

test('offline bound profile plus exactly one other online profile points explicitly to verified rebind', async t => {
  const f = await fixture(t); f.config.providers.feishu.profileId = 'cd';
  await atomicPrivateJson(path.join(f.root, 'config.json'), f.config);
  const result = await doctor(f.root, {...f, provider: 'feishu'});
  assert.equal(result.checks[1].code, 'extension-offline'); assert.deepEqual(result.profiles, ['ab']);
  assert.match(result.checks[1].nextStep, /Exactly one other profile.*bind --provider feishu --rebind/);
  assert.deepEqual(f.calls, []);
});

import assert from 'node:assert/strict'
import {spawnSync} from 'node:child_process'
import {fileURLToPath} from 'node:url'
import test from 'node:test'
import {validateResult, runChat} from '../../src/chat/tools.js'
import {mkdtemp, writeFile, rm} from 'node:fs/promises'
import {tmpdir} from 'node:os'
import {join} from 'node:path'

const source = fileURLToPath(new URL('../../src/', import.meta.url))
const code = `import json,sys
from unittest.mock import patch
sys.path.insert(0,sys.argv[1])
from chat import live,cli
from chat.extension import ExtensionError
browser={'transport':'chrome-extension','target_url':'https://fixture.feishu.cn/next/','allowed_origins':['https://fixture.feishu.cn'],'profile_id':'a'*32,'account_hash':'a'*64,'tenant_hash':'b'*64,'build_id':'c'*64}
binding={'browser':browser}
base={'ready':True,'profileId':'a'*32,'accountHash':'a'*64,'tenantHash':'b'*64,'capabilities':{'catalog':True},'diagnostics':{'catalogCount':2,'identityPresent':True,'buildQualified':False,'behavioralContractPassed':False,'providerDegraded':True,'ownedTabClosed':True,'conflictingTabCount':1,'bundleNames':['index.abcdef01.js','https://PRIVATE/'],'body':'PRIVATE_BODY','accountId':'PRIVATE_ACCOUNT','bootstrap':{'globals':[{'name':'PRIVATE_ACCOUNT','fields':[{'key':'private-secret','type':'string','value':'PRIVATE_BODY'}]}],'scriptResources':['https://secret/?token=PRIVATE_TOKEN']}}}
outputs=[]
for result in [base,{**base,'ready':False,'reason':'identity-unverified'},{**base,'capabilities':{}},{**base,'profileId':'other'},{**base,'accountHash':'c'*64},{'ready':'wrong'},ExtensionError('extension-unavailable','PRIVATE_RAW_ERROR')]:
    with patch('chat.extension.request',side_effect=result if isinstance(result,Exception) else None,return_value=result):
        outputs.append(live.status('feishu',binding)['liveDom'])
        if isinstance(result,ExtensionError):
            outputs.append(cli.statuses({'feishu':binding},'feishu')['providers'][0]['modes']['live-dom']['extension'])
outputs.append(live.status('feishu',{})['liveDom'])
outputs.append(live.status('feishu',{'browser':{**browser,'transport':'chrome-tab'}})['liveDom'])
print(json.dumps(outputs))`
const envelope = (rows, provider) => ({schemaVersion:1,operation:'status',exitCode:0,provider,data:{providers:rows}})
const row = (provider, extension) => ({provider,modes:{'live-dom':{configured:true,available:false,validation:'unverified',diagnostic:'browser-session-not-validated',...(extension ? {extension} : {})}}})

test('all live.py DOM paths cross the Node validator, including ExtensionError and ChatLiveError', () => {
  const proc = spawnSync('python3', ['-c',code,source], {encoding:'utf8',env:{...process.env,PYTHONDONTWRITEBYTECODE:'1'}})
  assert.equal(proc.status,0,proc.stderr)
  const outputs=JSON.parse(proc.stdout)
  assert.equal(outputs.length,10)
  for (const extension of outputs) {
    for (const key of ['state','reason','transport','targetConfigured','capabilities','diagnostics']) assert.ok(Object.hasOwn(extension,key))
    const result=validateResult('status',{provider:'feishu'},envelope([row('feishu',extension)],'feishu'))
    assert.equal(result.data.providers[0].error,undefined,JSON.stringify(extension))
  }
  assert.equal(outputs[6].reason,'extension-unavailable')
  assert.equal(outputs[8].transport,null)
  assert.deepEqual(outputs[0].diagnostics.bootstrap.fieldTypes,{string:1})
  assert.deepEqual(outputs[0].diagnostics.bundleNames, ['index.abcdef01.js'])
  assert.equal(outputs[0].diagnostics.conflictingTabCount, 1)
  assert.equal(outputs[0].diagnostics.behavioralContractPassed, false)
  assert.doesNotMatch(JSON.stringify(outputs),/PRIVATE_|private-secret|https:\/\/secret/)
})

test('one malformed provider is isolated while envelope and provider identities stay strict', () => {
  const rows=[row('wechat'),row('feishu',{state:'unavailable'}),row('teams')]
  const result=validateResult('status',{},envelope(rows))
  assert.deepEqual(result.data.providers[0],rows[0]); assert.deepEqual(result.data.providers[2],rows[2])
  assert.equal(result.data.providers[1].error.message,'Incomplete Chrome extension status.')
  for(const bad of [rows.slice(1),[rows[0],rows[0],rows[2]],[row('unknown'),rows[1],rows[2]]]) assert.throws(()=>validateResult('status',{},envelope(bad)))
  assert.throws(()=>validateResult('status',{}, {...envelope(rows),raw:'secret'}))
  const leak=row('feishu',{state:'ready',reason:null,transport:'chrome-extension',targetConfigured:true,capabilities:{},diagnostics:{body:'PRIVATE_BODY'}})
  const failed=validateResult('status',{provider:'feishu'},envelope([leak],'feishu'))
  assert.equal(failed.data.providers[0].error.code,'invalid-provider-output')
  assert.doesNotMatch(JSON.stringify(failed),/PRIVATE_BODY/)
})

test('runChat surfaces validator text but never JSON parser excerpts',async t=>{
  const dir=await mkdtemp(join(tmpdir(),'status-'));t.after(()=>rm(dir,{recursive:true,force:true}))
  const entry=join(dir,'reader.py')
  await writeFile(entry,`print('${JSON.stringify({schemaVersion:1,operation:'catalog',provider:'feishu',mode:'snapshot',exitCode:0,data:{entries:[],secret:'PRIVATE_RAW'}})}')`)
  const result=await runChat('catalog',{provider:'feishu',mode:'snapshot'},{entry})
  assert.equal(result.error.message,'Unexpected provider output fields.')
  await writeFile(entry,`print('PRIVATE_RAW malformed')`)
  const malformed=await runChat('status',{}, {entry})
  assert.equal(malformed.error.message,'Provider result is not valid JSON.')
})

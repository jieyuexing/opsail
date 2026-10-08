import assert from 'node:assert/strict';
import {mkdtemp, mkdir, writeFile, readFile, readdir, symlink, lstat, rm} from 'node:fs/promises';
import {tmpdir} from '../temp-root.mjs';
import path from 'node:path';
import test from 'node:test';
import {install} from '../../../src/browser/install.mjs';

test('upgrades retain a stable real directory, complete builds and last three releases', async t => {
  const root=await mkdtemp(path.join(tmpdir(),'i-')); t.after(()=>rm(root,{recursive:true,force:true}));
  const source=path.join(root,'source'); await mkdir(source);
  await writeFile(path.join(source,'manifest.json'), JSON.stringify({key:Buffer.from('fixture-public-key').toString('base64')}));
  const dataRoot=path.join(root,'data'); let previous;
  for(let n=0;n<5;n++) {
    await writeFile(path.join(source,'worker.js'), `// build ${n}`);
    const result=await install({dataRoot,extensionSource:source,writeRegistry:false});
    if(previous) assert.equal(result.unpackedPath,previous.unpackedPath);
    assert.equal((await lstat(result.unpackedPath)).isSymbolicLink(),false);
    assert.equal(await readFile(path.join(result.unpackedPath,'worker.js'),'utf8'),`// build ${n}`);
    assert.equal(JSON.parse(await readFile(path.join(result.unpackedPath,'build.json'),'utf8')).buildId,result.buildId);
    assert.match(await readFile(path.join(result.unpackedPath,'build.js'),'utf8'),new RegExp(`"buildId":"${result.buildId}"`));
    assert.equal((await readdir(path.join(dataRoot,'runtime/releases'))).length,Math.min(n+1,3));
    previous=result;
  }
  await rm(previous.unpackedPath,{recursive:true});
  await symlink(source,previous.unpackedPath);
  await assert.rejects(install({dataRoot,extensionSource:source,writeRegistry:false}),{code:'unsafe-path'});
  assert.equal((await lstat(previous.unpackedPath)).isSymbolicLink(),true);
});

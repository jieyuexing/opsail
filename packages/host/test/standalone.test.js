import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import { cpSync, mkdtempSync, mkdirSync, readFileSync, readdirSync, rmSync, statSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import test from 'node:test'

const packageRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const publicNames = ['opsail_read', 'opsail_xlsx_inspect', 'opsail_xlsx_patch', 'opsail_xlsx_diff', 'opsail_usage', 'opsail_chat_status', 'opsail_chat_catalog', 'opsail_chat_read']

function fixture(t) {
  const root = mkdtempSync(join(tmpdir(), 'opsail-public-'))
  t.after(() => rmSync(root, { recursive: true, force: true }))
  const pkg = join(root, 'package')
  mkdirSync(pkg)
  for (const name of ['package.json', 'pin.json', 'src', 'bin']) cpSync(join(packageRoot, name), join(pkg, name), { recursive: true })
  return { root, pkg }
}

function restrictedNode(pkg, root, args, input = '') {
  return execFileSync(process.execPath, ['--permission', `--allow-fs-read=${root}`, ...args], {
    cwd: pkg, input, encoding: 'utf8', timeout: 15_000,
    env: { PATH: process.env.PATH, HOME: root, XDG_DATA_HOME: join(root, 'data'),
      // A stale private-instance variable must not be consulted by public code.
      JIEYUEXING_UNIVERSE_ROOT: join(root, 'no-universe') },
    stdio: ['pipe', 'pipe', 'pipe'],
  })
}

test('isolation rejects a deliberate read outside the package and empty data fixture', t => {
  const { root, pkg } = fixture(t)
  const code = `import { readFileSync } from 'node:fs'; readFileSync(${JSON.stringify(join(packageRoot, 'package.json'))})`
  assert.throws(() => restrictedNode(pkg, root, ['--input-type=module', '-e', code]),
    error => error.status !== 0 && /ERR_ACCESS_DENIED/.test(error.stderr))
})

test('standalone package starts with access restricted to package and empty data fixture', t => {
  const { root, pkg } = fixture(t)
  const requests = [
    { jsonrpc: '2.0', id: 1, method: 'initialize', params: { protocolVersion: '2025-11-25' } },
    { jsonrpc: '2.0', id: 2, method: 'tools/list' },
    { jsonrpc: '2.0', id: 3, method: 'tools/call', params: { name: 'instance_only_tool', arguments: {} } },
  ]
  const lines = restrictedNode(pkg, root, ['src/mcp-server.js'], requests.map(value => JSON.stringify(value)).join('\n') + '\n')
    .trim().split('\n').map(line => JSON.parse(line))
  assert.equal(lines[0].result.serverInfo.name, 'opsail-host')
  assert.deepEqual(lines[1].result.tools.map(tool => tool.name), publicNames)
  assert.equal(lines[1].result.tools.find(tool => tool.name === 'opsail_xlsx_patch').annotations.readOnlyHint, false)
  assert.equal(lines[2].error.code, -32602)
})

test('public layout uses explicit paths without consulting a private instance', t => {
  const { root, pkg } = fixture(t)
  const result = JSON.parse(restrictedNode(pkg, root, ['--input-type=module', '-e',
    "import { resolveOpsailLayout } from './src/layout.js'; console.log(JSON.stringify(resolveOpsailLayout()))"]))
  assert.equal(result.pinPath, join(pkg, 'pin.json'))
  assert.equal(result.retainedChatDataDir, join(root, 'data', 'opsail-host', 'retained-chat'))
  assert.equal(result.chatBindingFile, join(result.retainedChatDataDir, 'bindings.json'))
})

test('package source is self-contained and excludes private configuration files', t => {
  const { pkg } = fixture(t)
  function inspect(dir) {
    for (const name of readdirSync(dir)) {
      const path = join(dir, name)
      const info = statSync(path)
      if (info.isDirectory()) inspect(path)
      else {
        assert.ok(!['bindings.json', 'daily.json', '.env', 'credential-binding.json'].includes(name), name)
        if (!/\.(js|mjs)$/.test(name)) continue
        const text = readFileSync(path, 'utf8')
        for (const match of text.matchAll(/(?:from\s*|import\s*\(\s*|import\s*)['"](\.[^'"]+)['"]/g)) {
          const target = resolve(dirname(path), match[1])
          assert.ok(target.startsWith(pkg + '/'), `${path} imports outside its package: ${match[1]}`)
        }
      }
    }
  }
  inspect(join(pkg, 'src'))
})

/** Pinned Opsail CLI resolution, argument validation, and execution. */
import { execFile } from 'node:child_process'
import { existsSync, readFileSync, realpathSync } from 'node:fs'
import { stat } from 'node:fs/promises'
import { isAbsolute, join } from 'node:path'
import { promisify } from 'node:util'

import { OPSAIL_PACKAGE_DIR, resolveOpsailLayout } from './layout.js'

const PACKAGE_DIR = OPSAIL_PACKAGE_DIR
export const runFile = promisify(execFile)

export const TOOL_TIMEOUT_MS = 120_000
export const OPSAIL_READ_MAX_INPUT_BYTES = 512 * 1024 * 1024
const OPSAIL_READ_MAX_OUTPUT_BYTES = 16 * 1024 * 1024
const BINARY_NAMES = process.platform === 'win32' ? ['opsail.exe', 'opsail'] : ['opsail']

export function readPin() {
  return JSON.parse(readFileSync(resolveOpsailLayout().pinPath, 'utf8'))
}

export function parseOpsailVersion(text) {
  const match = String(text ?? '').match(/\b(\d+\.\d+\.\d+)\b/)
  return match ? match[1] : null
}

export function resolveOpsailBinary(env = process.env) {
  const configured = env.OPSAIL_BINARY_PATH
  if (typeof configured === 'string' && configured.length > 0) {
    return existsSync(configured) ? configured : null
  }
  const pin = readPin()
  if (typeof pin.binary === 'string' && pin.binary.length > 0) {
    const path = isAbsolute(pin.binary) ? pin.binary : join(PACKAGE_DIR, pin.binary)
    if (existsSync(path)) return path
  }
  for (const rel of ['target/release', 'target/debug']) {
    for (const bin of BINARY_NAMES) {
      const candidate = join(resolveOpsailLayout().sourceDir, rel, bin)
      if (existsSync(candidate)) return candidate
    }
  }
  return lookPath(env, join(PACKAGE_DIR, 'bin', 'opsail'))
}

// PATH 中可能有指向本包 bin/opsail 的链接；跳过它，避免入口自我循环。
function lookPath(env, entry) {
  const path = env.PATH || env.Path || ''
  const sep = process.platform === 'win32' ? ';' : ':'
  const self = realPath(entry)
  for (const dir of path.split(sep)) {
    if (!dir) continue
    for (const bin of BINARY_NAMES) {
      const candidate = join(dir, bin)
      if (existsSync(candidate) && realPath(candidate) !== self) return candidate
    }
  }
  return null
}

function realPath(path) {
  try {
    return realpathSync(path)
  } catch {
    return path
  }
}

export function readArgv(args = {}) {
  const argv = ['read']
  const format = args.format
  if (format && format !== 'markdown') argv.push('--format', String(format))
  if (args.property) argv.push('--property', String(args.property))
  if (args.launch === true) argv.push('--launch')
  if (args.timeout != null && args.timeout !== '') argv.push('--timeout', String(args.timeout))
  const ranges = args.ranges
  if (ranges != null) {
    if (!Array.isArray(ranges) || ranges.length === 0 || ranges.length > 32) {
      throw new Error('cli.opsail: ranges must be an array of 1 to 32 XLSX selectors')
    }
    for (const range of ranges) {
      if (typeof range !== 'string' || range.length === 0 || range.length > 256) {
        throw new Error('cli.opsail: each XLSX range must be a bounded Sheet!A1:D20 string')
      }
      argv.push('--range', range)
    }
  }
  if (args.maxCells != null && args.maxCells !== '') {
    argv.push(
      '--max-cells',
      String(boundedInteger(args.maxCells, null, 1, 100_000, 'maxCells')),
    )
  }
  if (args.maxBytes != null && args.maxBytes !== '') {
    argv.push(
      '--max-bytes',
      String(
        boundedInteger(
          args.maxBytes,
          null,
          1,
          OPSAIL_READ_MAX_INPUT_BYTES,
          'maxBytes',
        ),
      ),
    )
  }
  if (args.maxExpandedBytes != null && args.maxExpandedBytes !== '') {
    argv.push(
      '--max-expanded-bytes',
      String(
        boundedInteger(
          args.maxExpandedBytes,
          null,
          1,
          512 * 1024 * 1024,
          'maxExpandedBytes',
        ),
      ),
    )
  }
  if (args.includeFormulas === false) argv.push('--no-formulas')
  if (args.revisionOnly === true) argv.push('--revision-only')
  if (args.revisionOnly === true && ranges != null) {
    throw new Error('cli.opsail: revisionOnly cannot be combined with XLSX ranges')
  }
  if (args.launch === true && ranges != null) {
    throw new Error('cli.opsail: XLSX ranges cannot be combined with Chrome launch')
  }
  if (args.launch === true && args.revisionOnly === true) {
    throw new Error('cli.opsail: revisionOnly cannot be combined with Chrome launch')
  }
  const source = args.source
  if (typeof source !== 'string' || source.length === 0) {
    throw new Error('cli.opsail: read needs a source URL, file path, or "-"')
  }
  if (/\.xlsx$/i.test(source)) {
    argv.push('--max-output-bytes', String(OPSAIL_READ_MAX_OUTPUT_BYTES))
  }
  const cookieFile = process.env.OPSAIL_COOKIE_FILE
  if (
    /^https?:\/\//i.test(source)
    && typeof cookieFile === 'string'
    && cookieFile.length > 0
  ) {
    argv.push('--cookie-file', cookieFile)
  }
  argv.push(source)
  return argv
}

export function boundedInteger(value, fallback, minimum, maximum, label) {
  if (value == null || value === '') return fallback
  const parsed = Number(value)
  if (!Number.isInteger(parsed) || parsed < minimum || parsed > maximum) {
    throw new Error(`cli.opsail: ${label} must be an integer from ${minimum} to ${maximum}`)
  }
  return parsed
}

export function assertSourceCheckout() {
  if (!existsSync(join(resolveOpsailLayout().sourceDir, 'Cargo.toml'))) {
    throw new Error('cli.opsail: native source checkout missing; restore the Opsail Host source checkout')
  }
}

/**
 * The pinned-version check stays per call, but spawning `--version` before
 * every tool invocation doubles process launches. Remember the answer per
 * binary inode/size/mtime/ctime so an unchanged binary is probed once.
 */
const binaryVersions = new Map()
async function probeBinaryVersion(binary) {
  let signature = null
  try {
    const info = await stat(binary)
    signature = `${info.ino}:${info.size}:${info.mtimeMs}:${info.ctimeMs}`
    const known = binaryVersions.get(binary)
    if (known && known.signature === signature) return known.version
  } catch { signature = null }
  const identity = await runFile(binary, ['--version'], { timeout: 5_000, maxBuffer: 64 * 1024 })
  const version = parseOpsailVersion(identity.stdout) || parseOpsailVersion(identity.stderr)
  if (signature && version) binaryVersions.set(binary, { signature, version })
  return version
}

export async function runOpsail(argv, exec = {}, env = process.env) {
  if (!Array.isArray(argv) || argv.some((item) => typeof item !== 'string')) {
    throw new Error('cli.opsail: argv must be strings')
  }
  const binary = resolveOpsailBinary(env)
  if (!binary) {
    return {
      exitCode: 127,
      stdout: '',
      stderr: 'cli.opsail: opsail binary not found; build opsail/target/release or set OPSAIL_BINARY_PATH',
    }
  }
  const pin = readPin()
  try {
    const version = await probeBinaryVersion(binary)
    if (pin.wanted && version !== pin.wanted) {
      return {
        exitCode: 78,
        stdout: '',
        stderr: `cli.opsail: binary ${binary} is ${version ?? 'unknown'}, wanted ${pin.wanted}`,
      }
    }
    const pending = runFile(binary, argv, {
      signal: exec.signal,
      timeout: TOOL_TIMEOUT_MS,
      maxBuffer: OPSAIL_READ_MAX_OUTPUT_BYTES,
    })
    if (typeof exec.input === 'string') {
      pending.child.stdin.on('error', () => {}) // A rejected/older CLI may close stdin before reading.
      pending.child.stdin.end(exec.input)
    }
    const result = await pending
    return { exitCode: 0, stdout: result.stdout, stderr: result.stderr }
  } catch (error) {
    if (error && typeof error === 'object' && 'stdout' in error) {
      const code = error.code
      return {
        exitCode: Number.isInteger(code) ? code : 1,
        stdout: String(error.stdout ?? ''),
        stderr: String(error.stderr ?? error.message ?? ''),
      }
    }
    throw error
  }
}

export const RESULT_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  properties: {
    exitCode: { type: 'integer' },
    stdout: { type: 'string' },
    stderr: { type: 'string' },
  },
  required: ['exitCode', 'stdout', 'stderr'],
}

export function renderCommand(command, value) {
  const output = [value.stdout, value.stderr].filter(Boolean).join('\n').trim()
  return [{ type: 'text', text: output || `${command} exited ${value.exitCode}` }]
}

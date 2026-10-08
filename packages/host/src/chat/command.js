#!/usr/bin/env node
/** Operator CLI; uses exactly the same bounded Node boundary as Host and MCP. */
import { parseArgs } from 'node:util'
import { pathToFileURL } from 'node:url'
import { runChat } from './tools.js'

const help = `Usage: chat status [--provider wechat|feishu|teams]
       chat check --provider PROVIDER --mode snapshot|live-dom [--quiet]
       chat catalog --provider PROVIDER --mode MODE [--limit N] [--cursor CURSOR]
       chat read --provider PROVIDER --mode MODE --conversation-id ID [--limit N] [--cursor CURSOR]
Selectors --conversation-name NAME and --conversation-url URL are also supported.
JSON goes to stdout. Errors go to stderr with exit 2. Quiet checks emit only errors.
Checks and reads never synchronize, navigate the browser, or send messages.\n`

export async function runCommand(argv, call = runChat) {
  let parsed
  try {
    parsed = parseArgs({ args: argv, allowPositionals: true, strict: true, options: {
      provider: { type: 'string' }, mode: { type: 'string' }, limit: { type: 'string' },
      cursor: { type: 'string' }, 'conversation-id': { type: 'string' },
      'conversation-name': { type: 'string' }, 'conversation-url': { type: 'string' },
      quiet: { type: 'boolean' }, help: { type: 'boolean' },
    } })
  } catch { return invalid() }
  const { values, positionals } = parsed
  if (values.help) return { code: 0, stdout: help, stderr: '' }
  const [operation] = positionals
  if (positionals.length !== 1 || !['status', 'check', 'catalog', 'read'].includes(operation)) return invalid()
  if (values.quiet && operation !== 'check') return invalid()
  const args = {}
  for (const [from, to] of Object.entries({ provider: 'provider', mode: 'mode', cursor: 'cursor', 'conversation-id': 'conversationId', 'conversation-name': 'conversationName', 'conversation-url': 'conversationUrl' })) {
    if (values[from] !== undefined) args[to] = values[from]
  }
  if (values.limit !== undefined) {
    if (!/^[1-9][0-9]*$/.test(values.limit)) return invalid()
    args.limit = Number(values.limit)
  }
  let result
  if (operation === 'check') {
    if (!['wechat', 'feishu', 'teams'].includes(args.provider) || !['snapshot', 'live-dom', 'live-api'].includes(args.mode) || Object.keys(args).some(key => !['provider', 'mode'].includes(key))) return invalid()
    const status = await call('status', { provider: args.provider })
    if (status.exitCode !== 0) result = status
    else {
      const mode = status.data.providers[0].modes[args.mode]
      if (!mode.available || mode.freshness?.coverage?.complete === false) {
        result = { schemaVersion: 1, operation: 'check', exitCode: 2, error: { code: mode.diagnostic, message: 'Selected source is not ready; inspect status for freshness and coverage.' }, data: { provider: args.provider, mode: args.mode, readiness: mode }, implementation: status.implementation }
      } else {
        const probe = await call('catalog', { ...args, limit: 1 })
        result = probe.exitCode !== 0 ? probe : { schemaVersion: 1, operation: 'check', exitCode: 0, data: { provider: args.provider, mode: args.mode, readiness: mode, catalogReadable: true }, implementation: probe.implementation }
      }
    }
  } else result = await call(operation, args)
  const code = result.exitCode === 0 ? 0 : 2
  const text = JSON.stringify(result) + '\n'
  return { code, stdout: code === 0 && !values.quiet ? text : '', stderr: code ? text : '' }
}

function invalid() {
  return { code: 2, stdout: '', stderr: JSON.stringify({ schemaVersion: 1, exitCode: 2, error: { code: 'invalid-request', message: 'Invalid chat CLI arguments. Use --help.' } }) + '\n' }
}

export async function main(argv = process.argv.slice(2)) {
  try {
    const result = await runCommand(argv)
    process.stdout.write(result.stdout); process.stderr.write(result.stderr); process.exitCode = result.code
  } catch {
    process.stderr.write(JSON.stringify({ schemaVersion: 1, exitCode: 2, error: { code: 'provider-unavailable', message: 'Chat CLI could not complete the request.' } }) + '\n')
    process.exitCode = 2
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) await main()

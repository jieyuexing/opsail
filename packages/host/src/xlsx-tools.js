/** Host/MCP projection of the native XLSX candidate-editing protocol. */
import { createHash } from 'node:crypto'
import { readFileSync } from 'node:fs'
import { isAbsolute } from 'node:path'
import { fileURLToPath } from 'node:url'

const MAX_BYTES = 512 * 1024 * 1024
const MAX_REQUEST_BYTES = 1024 * 1024
const string = { type: 'string', minLength: 1 }
const cell = { type: 'string', pattern: '^[A-Z]{1,3}[1-9][0-9]{0,6}$' }
const range = { type: 'string', maxLength: 256, minLength: 1 }
const colour = { type: 'string', pattern: '^[0-9A-Fa-f]{6}([0-9A-Fa-f]{2})?$' }
const object = (properties, required) => ({ type: 'object', additionalProperties: false, properties, required })
const style = object({
  fontColor: colour, fillColor: colour, bold: { type: 'boolean' }, strike: { type: 'boolean' },
  wrapText: { type: 'boolean' },
  horizontal: { type: 'string', enum: ['general', 'left', 'center', 'right', 'fill', 'justify', 'centerContinuous', 'distributed'] },
  vertical: { type: 'string', enum: ['top', 'center', 'bottom', 'justify', 'distributed'] },
  indent: { type: 'integer', minimum: 0, maximum: 250 },
}, [])
const operation = (op, properties, required) => object({ op: { const: op, type: 'string' }, sheet: string, ...properties }, ['op', 'sheet', ...required])
const operations = {
  type: 'array', minItems: 1, maxItems: 256,
  items: { oneOf: [
    operation('setText', { cell, expectedText: { type: 'string' }, value: { type: 'string' }, replaceRichText: { type: 'boolean' } }, ['cell', 'expectedText', 'value']),
    operation('setRichText', { cell, expectedText: { type: 'string' }, runs: { type: 'array', minItems: 1, maxItems: 256, items: object({ text: string, fontColor: colour, bold: { type: 'boolean' }, strike: { type: 'boolean' } }, ['text']) } }, ['cell', 'expectedText', 'runs']),
    operation('setNumber', { cell, expectedText: { type: 'string' }, value: { type: 'number' } }, ['cell', 'expectedText', 'value']),
    operation('setFormula', { cell, expectedText: { type: 'string' }, formula: { type: 'string', minLength: 1, maxLength: 8192 } }, ['cell', 'expectedText', 'formula']),
    operation('appendText', { cell, expectedText: { type: 'string' }, value: string, fontColor: colour, bold: { type: 'boolean' }, strike: { type: 'boolean' } }, ['cell', 'expectedText', 'value']),
    operation('setStyle', { range, style }, ['range', 'style']),
    operation('copyStyle', { range, fromCell: cell, components: { type: 'array', minItems: 1, uniqueItems: true, items: { type: 'string', enum: ['font', 'fill', 'border', 'alignment', 'numberFormat'] } } }, ['range', 'fromCell', 'components']),
    operation('rowHeight', { row: { type: 'integer', minimum: 1, maximum: 1048576 }, height: { type: 'number', minimum: 0, maximum: 409 } }, ['row', 'height']),
    operation('columnWidth', { column: { type: 'string', pattern: '^[A-Z]{1,3}$' }, width: { type: 'number', minimum: 0, maximum: 255 } }, ['column', 'width']),
    operation('rowVisibility', { row: { type: 'integer', minimum: 1, maximum: 1048576 }, hidden: { type: 'boolean' } }, ['row', 'hidden']),
    operation('insertRows', { before: { type: 'integer', minimum: 1, maximum: 1048576 }, count: { type: 'integer', minimum: 1, maximum: 500 }, styleFrom: { type: 'string', enum: ['above', 'none'] } }, ['before', 'count']),
  ] },
}
const limits = {
  maxBytes: { type: 'integer', minimum: 1, maximum: MAX_BYTES, description: 'Compressed input limit. Default 64 MiB.' },
  maxExpandedBytes: { type: 'integer', minimum: 1, maximum: MAX_BYTES, description: 'Cumulative expanded package limit. Default 256 MiB.' },
}
const maxCells = { type: 'integer', minimum: 1, maximum: 2000, description: 'Maximum detailed cells returned; default 200. Diff totals cover the whole bounded workbook.' }
const IMPLEMENTATION = Object.freeze({
  protocolVersion: 3,
  pluginVersion: process.env.OPSAIL_PLUGIN_VERSION || 'host-local',
  adapterSha256: createHash('sha256').update(readFileSync(fileURLToPath(import.meta.url))).digest('hex'),
})

function validate(operation, args, schema) {
  if (!args || typeof args !== 'object' || Array.isArray(args)) throw new Error('XLSX arguments must be an object')
  for (const name of Object.keys(args)) {
    if (!Object.hasOwn(schema.properties, name)) throw new Error(`Unknown XLSX argument: ${name}`)
  }
  for (const name of schema.required) {
    if (operation === 'patch' && name === 'output' && args.validateOnly === true) continue
    if (!Object.hasOwn(args, name)) throw new Error(`Missing XLSX argument: ${name}`)
  }
  for (const name of operation === 'diff' ? ['before', 'after'] : ['source', ...(operation === 'patch' && Object.hasOwn(args, 'output') ? ['output'] : [])]) {
    if (typeof args[name] !== 'string' || !isAbsolute(args[name]) || args[name].includes('\0')) throw new Error(`${name} must be an absolute local path`)
  }
  for (const name of ['maxCells', 'maxBytes', 'maxExpandedBytes']) {
    if (args[name] !== undefined && (!Number.isInteger(args[name]) || args[name] < 1 || args[name] > (name === 'maxCells' ? 2000 : MAX_BYTES))) throw new Error(`${name} is outside its supported bounds`)
  }
  for (const name of ['includeParts', 'validateOnly', 'semantic', 'alignRows']) {
    if (args[name] !== undefined && typeof args[name] !== 'boolean') throw new Error(`${name} must be a boolean`)
  }
  if (Object.hasOwn(args, 'alignRows') && args.semantic !== true) throw new Error('alignRows requires semantic:true')
  if (operation === 'inspect') {
    if (!Array.isArray(args.ranges) || args.ranges.length < 1 || args.ranges.length > 32 || args.ranges.some(r => typeof r !== 'string' || r.length < 1 || r.length > 256)) throw new Error('ranges must contain 1-32 bounded XLSX selectors')
    if (args.detail !== undefined && !['compact', 'full'].includes(args.detail)) throw new Error('detail must be compact or full')
    args = { ...args, detail: args.detail ?? 'compact', includeParts: args.includeParts ?? false }
  }
  if (operation === 'patch') {
    if (!/^[0-9a-f]{64}$/i.test(args.expectedSha256)) throw new Error('expectedSha256 must be a whole-file SHA-256')
    if (args.source === args.output) throw new Error('patch output must be a new candidate, separate from source')
    if (!Array.isArray(args.operations) || args.operations.length < 1 || args.operations.length > 256) throw new Error('operations must contain 1-256 bounded edits')
    if (args.operations.some(edit => edit?.op === 'setText' && edit.replaceRichText !== undefined && typeof edit.replaceRichText !== 'boolean')) throw new Error('replaceRichText must be a boolean')
    if (args.operations.some(edit => edit?.op === 'setRichText' && (!Array.isArray(edit.runs) || edit.runs.length < 1 || edit.runs.length > 256 || edit.runs.some(run => !run || typeof run.text !== 'string' || !run.text.length || ['bold', 'strike'].some(key => run[key] !== undefined && typeof run[key] !== 'boolean') || (run.fontColor !== undefined && !/^[0-9a-f]{6}([0-9a-f]{2})?$/i.test(run.fontColor))) || edit.runs.reduce((n, run) => n + run.text.length, 0) > 32767))) throw new Error('setRichText needs 1-256 nonempty text runs, valid font overrides and at most 32767 UTF-16 units')
    if (args.operations.some(edit => edit?.op === 'setNumber' && !Number.isFinite(edit.value))) throw new Error('setNumber value must be a finite number')
    if (args.operations.some(edit => edit?.op === 'insertRows' && (!Number.isInteger(edit.before) || edit.before < 1 || edit.before > 1048576 || !Number.isInteger(edit.count) || edit.count < 1 || edit.count > 500 || (edit.styleFrom !== undefined && !['above', 'none'].includes(edit.styleFrom))))) throw new Error('insertRows needs integer before 1-1048576, count 1-500 and styleFrom above|none')
  }
  const input = JSON.stringify({ schemaVersion: 1, operation, ...args })
  if (Buffer.byteLength(input) > MAX_REQUEST_BYTES) throw new Error('XLSX request exceeds 1 MiB')
  return input
}

export function createXlsxToolDefinitions({ runner }) {
  const contracts = [
    ['inspect', 'Inspect XLSX formatting',
      'Inspect exact XLSX ranges, dimensions, merges and hidden state with whole-file SHA-256. Defaults to compact resolved style summaries without styleContext or parts; use detail:"full" for stored style records and includeParts:true for package parts. Does not render, recalculate or certify visual layout.',
      object({ source: { ...string, description: 'Absolute path to the saved .xlsx workbook.' }, ranges: { type: 'array', minItems: 1, maxItems: 32, items: range, description: "Sheet-qualified A1 selectors such as Summary!A1:D20 or 'API Sheet'!B6:AY40." }, detail: { type: 'string', enum: ['compact', 'full'] }, includeParts: { type: 'boolean' }, maxCells, ...limits }, ['source', 'ranges'])],
    ['patch', 'Create XLSX patch candidate',
      'Apply authorized setText, setRichText, setNumber, setFormula, appendText and local formatting edits with an exact source SHA-256. appendText adds a styled run: plain old text keeps the cell font, existing rich-text runs stay unchanged and the new run starts from the last run font; phonetic annotations are refused. setFormula writes a formula and marks the workbook for full recalculation on open; stored results appear only after the target app recalculates. Missing target cells/rows are created with row/column style defaults. insertRows inserts whole rows above `before` (styleFrom above copies the previous row height and cell styles), moving cells, merges, drawing anchors, defined names and references to the sheet like Excel; later operations use the new row numbers; shared/array formulas or tables at the insertion point are refused. copyStyle with all five components adopts the donor whole style including its base named style; partial copies across different base styles are refused. validateOnly:true checks a dry run without writes; output is then optional. Otherwise creates a new candidate, preserving untouched parts and never overwriting source or output. setRichText replaces all text with 1-256 new runs using the cell font plus fontColor/bold/strike overrides. setText replaceRichText:true replaces rich text with plain text using the cell font, including strike=1; use setRichText to override strike. No row deletion, column insertion, merge changes, shared/array formula editing, or preserving existing runs while restyling a selected run. Product rules decide colors; visual acceptance needs the target app.',
      object({ source: { ...string, description: 'Absolute path to the saved .xlsx workbook to patch.' }, output: { ...string, description: 'Absolute path for the new candidate workbook; must differ from source and is never overwritten. Optional when validateOnly is true.' }, expectedSha256: { type: 'string', pattern: '^[0-9A-Fa-f]{64}$', description: 'Whole-file SHA-256 of the saved source as returned by opsail_xlsx_inspect; binds the patch to that exact file.' }, operations, validateOnly: { type: 'boolean' }, ...limits }, ['source', 'output', 'expectedSha256', 'operations'])],
    ['diff', 'Compare XLSX storage and styles',
      'Compare two XLSX files by package content and stored cell semantics, resolving style components instead of comparing style IDs. Use semantic:true to identify user edits after a WPS save: normalized cell changes, layoutChanges and workbookChanges separate from equivalentOnly and viewOnly. Add alignRows:true only with semantic:true when rows moved or were inserted. Returns full bounded-workbook totals with capped details; check all truncated flags and outputTruncated and rerun with higher maxCells or bounded batches before concluding. Unknown parts and native objects remain separately reported; this is not WPS visual, print or business acceptance.',
      object({ before: { ...string, description: 'Absolute path to the original workbook.' }, after: { ...string, description: 'Absolute path to the changed workbook, such as a patch output.' }, semantic: { type: 'boolean' }, alignRows: { type: 'boolean', description: 'Requires semantic:true; align rows by content before comparing cells and merges.' }, maxCells, ...limits }, ['before', 'after'])],
  ]
  return contracts.map(([operation, title, description, parameters]) => ({
    name: `opsail_xlsx_${operation}`, title, description, parameters,
    annotations: { readOnlyHint: operation !== 'patch', destructiveHint: false, idempotentHint: operation !== 'patch', openWorldHint: false },
    timeoutMs: 120_000,
    output: {
      schema: { type: 'object', additionalProperties: true, properties: { exitCode: { type: 'integer' }, schemaVersion: { type: 'integer' }, implementation: { type: 'object', additionalProperties: true } }, required: ['exitCode', 'implementation'] },
      render: (_args, result) => [{ type: 'text', text: JSON.stringify(result, null, 2) }],
    },
    async execute(args, exec = {}) {
      const input = validate(operation, args, parameters)
      const command = await runner(['xlsx', '--machine'], { ...exec, input })
      let result
      try { result = JSON.parse(command.stdout) } catch {
        if (command.exitCode === 0) throw new Error('Opsail XLSX returned invalid JSON')
        result = { error: { message: String(command.stderr || 'Opsail XLSX capability unavailable').slice(0, 4000) } }
      }
      if (!result || typeof result !== 'object' || Array.isArray(result)) throw new Error('Opsail XLSX returned a non-object response')
      if (command.exitCode === 0 && result.schemaVersion !== 1) throw new Error('Opsail XLSX returned an unsupported protocol version')
      return { ...result, exitCode: command.exitCode, implementation: IMPLEMENTATION }
    },
  }))
}

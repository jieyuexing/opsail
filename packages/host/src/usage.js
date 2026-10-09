/** Credential-free Opsail usage projections and tool definition. */
import { renderCommand, RESULT_SCHEMA, runOpsail, TOOL_TIMEOUT_MS } from './runtime.js'

export const USAGE_PROVIDERS = new Set(['codex', 'grok', 'claude'])
const USAGE_FIELDS = [
  'usedPercent',
  'resetsAt',
  'windowDurationMins',
  'planType',
  'resetCreditAvailableCount',
  'resetCreditExpiresAt',
]

export function usageArgv(args = {}) {
  const argv = ['usage']
  if (args.provider != null && args.provider !== '') {
    const provider = String(args.provider)
    if (!USAGE_PROVIDERS.has(provider)) {
      throw new Error(`cli.opsail: unsupported usage provider ${provider}`)
    }
    argv.push(provider)
  }
  argv.push('--format', args.format === 'text' ? 'text' : 'json')
  if (args.timeout != null && args.timeout !== '') argv.push('--timeout', String(args.timeout))
  return argv
}

function unavailableUsage(detail = 'Opsail usage 暂不可用。') {
  return { status: 'unavailable', remainingPercent: null, detail }
}

/** 将桌面 get_usage 或 Opsail Claude 条目投影为无凭据的窗口快照，不作派发决策。 */
export function normalizeClaudeUsage(input) {
  const object = value => value !== null && typeof value === 'object' && !Array.isArray(value)
  const invalid = field => { throw new Error(`Claude 额度格式错误：${field}`) }
  if (!object(input)) invalid('需要对象')
  const entry = object(input.plan) ? input.plan : input
  if (!['ok', 'ready', 'unavailable', 'not_applicable'].includes(entry.status)) invalid('status')
  const resetFields = entry.unparsedResetFields
  if (resetFields != null && (!Array.isArray(resetFields) || !resetFields.every(field => typeof field === 'string'))) invalid('unparsedResetFields')
  const resetProjection = resetFields == null ? {} : { unparsedResetFields: [...resetFields] }
  if (entry.status === 'unavailable' || entry.status === 'not_applicable') {
    return {
      status: entry.status, remainingPercent: null, windows: [], ...resetProjection,
      detail: typeof entry.detail === 'string' ? entry.detail : `Claude 额度状态为 ${entry.status}。`,
    }
  }
  if (!Array.isArray(entry.windows)) invalid('windows')
  const percent = (value, field) => {
    if (value == null) return null
    if (!Number.isFinite(value) || value < 0 || value > 100) invalid(field)
    return value
  }
  const timestamp = (value, field = 'resetsAt') => {
    if (value == null) return null
    const millis = typeof value === 'number' ? value * 1000
      : typeof value === 'string' && /^\d{4}-\d{2}-\d{2}T.*(?:Z|[+-]\d{2}:\d{2})$/.test(value) ? Date.parse(value) : NaN
    if (!Number.isFinite(millis) || millis < 0 || !Number.isFinite(new Date(millis).getTime())) invalid(field)
    return millis / 1000
  }
  const ids = new Set()
  const windows = entry.windows.map(window => {
    if (!object(window)) invalid('window')
    if (window.label != null && typeof window.label !== 'string') invalid('label')
    if (window.id != null && (typeof window.id !== 'string' || !window.id)) invalid('id')
    const label = window.label ?? null
    let id = window.id ?? null
    if (id === null) {
      if (label === '5-hour limit') id = 'five_hour'
      else if (label === 'Weekly · all models') id = 'seven_day'
      else if (label?.startsWith('Weekly · ') && label.slice(9).trim()) {
        id = 'seven_day_' + label.slice(9).trim().toLowerCase().replace(/[^a-z0-9]/g, '_')
      }
    }
    if (id === null && !label) invalid('窗口需要 id 或 label')
    if (id !== null && ids.has(id)) invalid(`重复窗口 ${id}`)
    if (id !== null) ids.add(id)
    const used = percent(window.percentUsed ?? window.usedPercent, 'usedPercent')
    const remaining = percent(window.remainingPercent, 'remainingPercent')
    if (used === null && remaining === null) invalid('窗口缺少百分比')
    if (used !== null && remaining !== null && Math.abs(used + remaining - 100) > 1) invalid('窗口百分比冲突')
    const duration = window.windowDurationMins
      ?? (id === 'five_hour' ? 300 : id === 'seven_day' || id?.startsWith('seven_day_') ? 10080 : null)
    if (duration !== null && (!Number.isFinite(duration) || duration <= 0)) invalid('windowDurationMins')
    return {
      id, label, remainingPercent: remaining ?? 100 - used, usedPercent: used ?? 100 - remaining,
      resetsAt: timestamp(window.resetsAt), windowDurationMins: duration,
    }
  })
  const primary = windows.find(window => window.id === 'five_hour')
    ?? windows.find(window => window.id === 'seven_day') ?? windows[0]
  return {
    status: 'ready', remainingPercent: primary?.remainingPercent ?? null, ...resetProjection,
    usedPercent: primary?.usedPercent ?? null, resetsAt: primary?.resetsAt ?? null,
    windowDurationMins: primary?.windowDurationMins ?? null,
    planType: typeof entry.plan === 'string' ? entry.plan : typeof entry.planType === 'string' ? entry.planType : null,
    ...(entry.observedAt != null || input.observedAt != null
      ? { observedAt: timestamp(entry.observedAt ?? input.observedAt, 'observedAt') } : {}),
    windows,
  }
}

export function parseUsageSnapshot(text, provider) {
  if (!USAGE_PROVIDERS.has(provider)) {
    throw new Error(`cli.opsail: unsupported usage provider ${provider}`)
  }
  const report = JSON.parse(text)
  if (report?.schemaVersion !== 1 || !Array.isArray(report.providers)) {
    throw new Error('cli.opsail: opsail usage returned an unsupported schema')
  }
  const entry = report.providers.find((item) => item?.provider === provider)
  if (!entry || entry.status !== 'ready' || !Number.isFinite(entry.remainingPercent)) {
    return unavailableUsage(
      typeof entry?.detail === 'string' && entry.detail.length > 0
        ? entry.detail
        : `Opsail usage 未返回 ${provider} 的可用窗口。`,
    )
  }
  const snapshot = {
    status: 'ready',
    remainingPercent: Math.min(100, Math.max(0, Math.round(entry.remainingPercent))),
  }
  for (const field of USAGE_FIELDS) {
    if (entry[field] != null) snapshot[field] = entry[field]
  }
  if (Array.isArray(entry.windows)) {
    const fields = ['id', 'label', 'remainingPercent', 'usedPercent', 'resetsAt', 'windowDurationMins']
    snapshot.windows = entry.windows.map((window) => Object.fromEntries(
      fields.filter((field) => window?.[field] != null).map((field) => [field, window[field]]),
    ))
  }
  return snapshot
}

/** Read one credential-free usage projection through the pinned Opsail CLI. */
export async function readOpsailUsage(provider, { runner = runOpsail, signal } = {}) {
  try {
    const result = await runner(usageArgv({ provider }), { signal })
    if (result.exitCode !== 0) return unavailableUsage()
    return parseUsageSnapshot(result.stdout, provider)
  } catch {
    return unavailableUsage()
  }
}

export function createOpsailUsageToolDefinition() {
  return {
    name: 'opsail_usage',
    title: 'Read Claude, Codex or Grok remaining usage',
    description:
      'Read credential-free Claude, Codex and Grok CLI remaining-usage windows with `opsail usage`.',
    parameters: {
      type: 'object',
      additionalProperties: false,
      properties: {
        provider: {
          type: 'string',
          enum: [...USAGE_PROVIDERS],
          description: 'Optional runtime to query. Omit to query all three.',
        },
        format: {
          type: 'string',
          enum: ['json', 'text'],
          description: 'Output representation. Default json.',
        },
        timeout: {
          type: 'integer',
          description: 'Overall provider query timeout in seconds.',
        },
      },
    },
    output: {
      schema: RESULT_SCHEMA,
      render: (_args, value) => renderCommand('opsail usage', value),
    },
    timeoutMs: TOOL_TIMEOUT_MS,
    async execute(args, exec) {
      return runOpsail(usageArgv(args), exec)
    },
  }
}

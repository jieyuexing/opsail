/** Public capability assembly. It never imports a private instance adapter. */
import { createOpsailReadToolDefinition, createOpsailXlsxToolDefinitions } from './read-tools.js'
import { createOpsailUsageToolDefinition } from './usage.js'
import { createOpsailChatToolDefinitions } from './chat/tools.js'
export { createOpsailReadToolDefinition, createOpsailXlsxToolDefinitions } from './read-tools.js'
export { createOpsailUsageToolDefinition, readOpsailUsage, normalizeClaudeUsage, parseUsageSnapshot, usageArgv } from './usage.js'
export { createOpsailChatToolDefinitions } from './chat/tools.js'
export { resolveOpsailLayout } from './layout.js'
export { parseOpsailVersion, readArgv, readPin, resolveOpsailBinary, runOpsail } from './runtime.js'

export function createPublicToolDefinitions() {
  return [createOpsailReadToolDefinition(), ...createOpsailXlsxToolDefinitions(), createOpsailUsageToolDefinition(), ...createOpsailChatToolDefinitions()]
}

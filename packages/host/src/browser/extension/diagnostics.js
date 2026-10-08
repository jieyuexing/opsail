import {FEISHU_BUILD} from './identity-builds.js'

const counts = ['catalogCount', 'stableIdCount', 'selectedCount', 'messageNodeCount', 'verifiedMessageScopeCount', 'stableMessageIdCount', 'messageRowScopeCount', 'messagePaneScopeCount', 'rowScopeMatchCount', 'paneScopeMatchCount', 'resolvedScopeMatchCount', 'displayedMessageCount', 'conflictingTabCount']
const flags = ['identityPresent', 'catalogIdentityVerified', 'messageScopeVerified', 'loginFormPresent', 'buildQualified', 'behavioralContractPassed', 'bundleNamesTruncated', 'providerDegraded', 'ownedTabClosed', 'ownedTabCloseFailed']
const bootstrapKeys = ['globals', 'componentShapes', 'stateShapes', 'conversationComparisons', 'identityShapes', 'clientShapes', 'selectionShapes', 'currentTreeShapes']
const types = ['string', 'number', 'boolean', 'object', 'undefined', 'function', 'symbol', 'bigint', 'null', 'accessor']
const count = v => Number.isInteger(v) && v >= 0 && v <= 1_000_000
const object = v => v && typeof v === 'object' && !Array.isArray(v)

// Project a fixed vocabulary on BOTH sides of Native Messaging. Never copy
// page-owned keys, URLs, IDs, exception text or arbitrary nested objects.
export function boundedDiagnostics(value) {
  const out = {}
  if (!object(value)) return out
  for (const key of counts) if (count(value[key])) out[key] = value[key]
  for (const key of flags) if (typeof value[key] === 'boolean') out[key] = value[key]
  if (['dom-metadata', 'teams-authenticated-context', 'feishu-session-user'].includes(value.identityKind)) out.identityKind = value.identityKind
  if (Array.isArray(value.bundleNames)) out.bundleNames = [...new Set(value.bundleNames.filter(name => typeof name === 'string' && FEISHU_BUILD.test(name)))].slice(0, 16)
  if (object(value.bootstrap)) {
    const bootstrap = value.bootstrap, summary = {fieldTypes: {}}
    let budget = 4000
    const visit = (node, depth = 0) => {
      if (depth > 8 || --budget < 0) return
      if (Array.isArray(node)) { for (const item of node.slice(0, 100)) visit(item, depth + 1) }
      else if (object(node)) {
        if (types.includes(node.type)) summary.fieldTypes[node.type] = (summary.fieldTypes[node.type] || 0) + 1
        for (const key of ['fields', 'children', 'slices', 'sampleFields', 'user', 'tenant', 'authenticationUser', 'profile', 'prototype', 'parentPrototype', 'innerClient', 'context']) if (key in node) visit(node[key], depth + 1)
      }
    }
    for (const key of bootstrapKeys) {
      if (Array.isArray(bootstrap[key])) { summary[key + 'Count'] = Math.min(bootstrap[key].length, 1000); visit(bootstrap[key]) }
      else if (count(bootstrap[key + 'Count'])) summary[key + 'Count'] = bootstrap[key + 'Count']
    }
    if (object(bootstrap.fieldTypes)) for (const key of types) if (count(bootstrap.fieldTypes[key])) summary.fieldTypes[key] = bootstrap.fieldTypes[key]
    out.bootstrap = summary
  }
  return out
}

export function identityNextStep(code, provider = 'feishu') {
  if (['provider-degraded', 'target-tab-conflict'].includes(code)) return 'Close other Feishu messenger tabs, then retry.'
  if (code === 'unqualified-build') return 'Review diagnostics.bundleNames, then run opsail-chrome qualify --provider feishu --build EXACT_BUNDLE_NAME; retry bind or doctor.'
  if (['identity-unverified', 'identity-read-failed', 'login-required', 'account-changed', 'tenant-changed'].includes(code)) return 'Sign in to the originally bound account and tenant, then retry.'
  if (['tab-not-prepared', 'tab-missing', 'document-changed', 'binding-changed', 'page-changed', 'load-timeout', 'injection-unavailable'].includes(code)) return `Run chat prepare --provider ${provider}, then retry.`
  return null
}

import {BridgeError} from '../protocol.js'

// Qualification is a deployed implementation capability, never a user-controlled flag.
// Add a read-only site driver here only together with response/identity/pagination evidence.
export const ADAPTER_VERSION = 'browser-sources-1'
export const REQUIRED_SURFACES = {
  feishu: ['chats', 'hidden-chats', 'archived-chats'],
  teams: ['chats', 'hidden-chats', 'archived-chats', 'meeting-chats', 'teams', 'channels', 'channel-threads'],
}
const drivers = Object.freeze({})
export function qualification(provider, identity) {
  const driver = drivers[provider]
  return {
    qualified: !!driver && !!identity.accountHash && !!identity.tenantHash,
    accountHash: identity.accountHash, tenantHash: identity.tenantHash, adapterVersion: ADAPTER_VERSION,
    capabilities: {catalog: !!driver, history: !!driver, channels: provider === 'teams' && !!driver, replies: !!driver},
    reason: driver ? (identity.accountHash ? null : 'identity-unverified') : 'browser-api-unqualified',
    surfaces: REQUIRED_SURFACES[provider].map(name => ({name, complete: false, reason: 'browser-api-unqualified'})),
  }
}
export async function apiPage(provider, operation, args, context) {
  const driver = drivers[provider]
  if (!driver) throw new BridgeError('browser-api-unqualified')
  return driver[operation](args, context)
}

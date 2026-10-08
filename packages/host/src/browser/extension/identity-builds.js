export const FEISHU_BUILD = /^index\.[a-f0-9]{6,64}\.js$(?![\s\S])/
export const DEFAULT_FEISHU_BUILDS = Object.freeze(['index.745e4057.js'])
export const MAX_IDENTITY_BUILDS = 64
export function defaultIdentityBuilds() {
  return {feishu: DEFAULT_FEISHU_BUILDS.map(name => ({name, qualification: 'built-in'}))}
}
export function validIdentityBuilds(value) {
  return value && !Array.isArray(value) && Object.keys(value).length === 1 &&
    Array.isArray(value.feishu) && value.feishu.length <= MAX_IDENTITY_BUILDS &&
    value.feishu.every(entry => entry && Object.keys(entry).length === 2 &&
      typeof entry.name === 'string' && FEISHU_BUILD.test(entry.name) &&
      ['built-in', 'behavior-verified', 'qualified-by-operator'].includes(entry.qualification)) &&
    new Set(value.feishu.map(entry => entry.name)).size === value.feishu.length
}
export function feishuBuildNames(config) {
  return (config.identityBuilds ?? defaultIdentityBuilds()).feishu.map(entry => entry.name)
}

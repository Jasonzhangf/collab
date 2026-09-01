export const WORKER_STATES = Object.freeze(['registered', 'active', 'paused', 'closed'])

export function assertWorkerRegistration(input) {
  if (!input || typeof input !== 'object') throw new TypeError('registration must be an object')
  for (const field of ['agentId', 'kind', 'cwd', 'panelId']) {
    if (typeof input[field] !== 'string' || input[field].length === 0) throw new TypeError(`${field} is required`)
  }
  if (!Array.isArray(input.capabilities)) throw new TypeError('capabilities must be an array')
  if (!Array.isArray(input.endpoints)) throw new TypeError('endpoints must be an array')
  return input
}

export function capabilityKey(capability) {
  if (typeof capability === 'string') return capability
  if (capability && typeof capability.id === 'string') return capability.id
  throw new TypeError('capability must be a string or { id }')
}

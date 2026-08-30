const NOTICE_TYPES = new Set(['occupied', 'released'])
const AGENT_STATES = new Set(['absent', 'unknown', 'working', 'waiting'])

export const CommunicationHub = (ctx, config = {}) => {
  const tmux = config.tmuxTransport ?? null
  const probe = config.agentStateProbe ?? (async () => 'unknown')
  const hub = Object.freeze({
    send(input) {
      if (!NOTICE_TYPES.has(input?.notice)) throw new TypeError('notice must be occupied or released')
      return ctx.collab.sendResourceNotice(input)
    },
    async wake(input) {
      if (typeof input?.messageId !== 'string' || input.messageId.length === 0) throw new TypeError('messageId is required')
      const snapshot = ctx.collab.snapshot()
      const message = snapshot.messages.find(({ id }) => id === input.messageId)
      if (!message) throw new Error(`unknown message: ${input.messageId}`)
      if (message.delivered || message.wake_attempt_count >= 3 || message.subscription_id === null) return ctx.collab.beginWakeAttempt({ messageId: input.messageId, agentState: 'unknown', nowMs: input.nowMs })
      const identity = snapshot.identities.find(({ id }) => id === message.to)
      if (!identity) throw new Error(`unknown target identity: ${message.to}`)
      const agentState = await probe(identity)
      if (!AGENT_STATES.has(agentState)) throw new Error(`invalid agent state: ${agentState}`)
      if (agentState !== 'waiting') return ctx.collab.beginWakeAttempt({ messageId: input.messageId, agentState, nowMs: input.nowMs })
      const lease = ctx.collab.beginWakeAttempt({ messageId: input.messageId, agentState, nowMs: input.nowMs })
      const attempt = ctx.collab.snapshot().messages.find(({ id }) => id === input.messageId).wake_attempt_count
      try {
        if (!tmux) throw new Error('tmux transport is unavailable')
        const receipt = await tmux.deliver({ target: identity.pane, messageId: input.messageId })
        const completion = ctx.collab.completeWakeAttempt({ messageId: input.messageId, attempt, succeeded: true })
        return Object.freeze({ lease, completion, receipt })
      } catch (error) {
        ctx.collab.completeWakeAttempt({ messageId: input.messageId, attempt, succeeded: false })
        throw error
      }
    },
  })
  ctx.provide('communication', hub)
  return async () => { await tmux?.close?.() }
}

CommunicationHub.inject = ['collab']
CommunicationHub.provide = 'communication'

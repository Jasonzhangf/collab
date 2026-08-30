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
      if (message.delivered || message.wake_attempt_count >= 3 || message.subscription_id === null) return ctx.collab.wakeAttempt({ messageId: input.messageId, agentState: 'unknown', succeeded: false, nowMs: input.nowMs })
      const identity = snapshot.identities.find(({ id }) => id === message.to)
      if (!identity) throw new Error(`unknown target identity: ${message.to}`)
      const agentState = await probe(identity)
      if (!AGENT_STATES.has(agentState)) throw new Error(`invalid agent state: ${agentState}`)
      if (agentState !== 'waiting') return ctx.collab.wakeAttempt({ messageId: input.messageId, agentState, succeeded: false, nowMs: input.nowMs })
      if (!tmux) throw new Error('tmux transport is unavailable')
      try {
        const receipt = await tmux.deliver({ target: identity.pane, messageId: input.messageId })
        const result = ctx.collab.wakeAttempt({ messageId: input.messageId, agentState, succeeded: true, nowMs: input.nowMs })
        return Object.freeze({ result, receipt })
      } catch (error) {
        ctx.collab.wakeAttempt({ messageId: input.messageId, agentState, succeeded: false, nowMs: input.nowMs })
        throw error
      }
    },
  })
  ctx.provide('communication', hub)
  return async () => { await tmux?.close?.() }
}

CommunicationHub.inject = ['collab']
CommunicationHub.provide = 'communication'

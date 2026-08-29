export const CommunicationHub = (ctx, config = {}) => {
  const transports = new Map(Object.entries(config.transports ?? {}))
  const deliveries = []
  let messageCounter = 0
  const lifecycleIntervalMs = config.lifecycleIntervalMs ?? 5000
  const idleClosed = new Set()
  const hub = {
    registerTransport(type, transport) {
      if (transports.has(type)) throw new Error(`transport already registered: ${type}`)
      if (!transport || typeof transport.deliver !== 'function') throw new TypeError(`transport ${type} must implement deliver`)
      transports.set(type, transport)
    },
    async send({ fromAgentId, toAgentId, payload }) {
      if (payload === undefined) throw new TypeError('payload is required')
      const messageId = `message-${Date.now()}-${++messageCounter}`
      const target = ctx.collab.listWorkers().find((worker) => worker.agentId === toAgentId)
      if (!target) throw new Error(`unknown target worker: ${toAgentId}`)
      const endpoint = target.endpoints.find((candidate) => transports.has(candidate.type))
      if (!endpoint) throw new Error(`no registered transport for target: ${toAgentId}`)
      const transport = transports.get(endpoint.type)
      idleClosed.delete(endpoint.type)
      ctx.collab.createMessage({ messageId, fromAgentId, toAgentId, payload })
      ctx.collab.transitionMessage(messageId, 'policy_checked')
      ctx.collab.transitionMessage(messageId, 'queued')
      if (target.presence.status === 'offline') {
        const deferred = ctx.collab.transitionMessage(messageId, 'deferred')
        const delivery = Object.freeze({ messageId, deliveryId: `${Date.now()}-${deliveries.length + 1}`, fromAgentId, toAgentId, transport: endpoint.type, status: deferred.state, receipt: null, deliveredAt: null })
        deliveries.push(delivery)
        return delivery
      }
      let receipt
      try {
        receipt = await transport.deliver({ messageId, fromAgentId, toAgentId, endpoint, payload })
        ctx.collab.transitionMessage(messageId, 'transport_accepted')
      } catch (error) {
        ctx.collab.transitionMessage(messageId, 'failed')
        throw error
      }
      const delivery = Object.freeze({ messageId, deliveryId: `${Date.now()}-${deliveries.length + 1}`, fromAgentId, toAgentId, transport: endpoint.type, receipt, deliveredAt: Date.now() })
      deliveries.push(delivery)
      return delivery
    },
    async receive({ agentId, endpoint }) {
      if (!endpoint || !transports.has(endpoint.type)) throw new Error(`no registered transport for endpoint: ${endpoint?.type}`)
      const transport = transports.get(endpoint.type)
      if (typeof transport.receive !== 'function') throw new Error(`transport does not support receive: ${endpoint.type}`)
      const records = await transport.receive({ agentId, endpoint })
      for (const record of records) {
        const message = ctx.collab.listMessages().find((candidate) => candidate.messageId === record.messageId)
        if (message?.state === 'queued' || message?.state === 'transport_accepted') ctx.collab.transitionMessage(record.messageId, 'arrived')
      }
      return records
    },
    async acknowledge({ messageId, agentId }) {
      const worker = ctx.collab.listWorkers().find((candidate) => candidate.agentId === agentId)
      const endpoint = worker?.endpoints.find((candidate) => transports.has(candidate.type))
      if (!endpoint) throw new Error(`no registered endpoint for worker: ${agentId}`)
      const transport = transports.get(endpoint.type)
      if (typeof transport.acknowledge !== 'function') throw new Error(`transport does not support acknowledge: ${endpoint.type}`)
      const receipt = await transport.acknowledge({ messageId, agentId, endpoint })
      ctx.collab.acknowledgeMessage(messageId)
      return receipt
    },
    listDeliveries: () => [...deliveries],
    reconcileLifecycle() {
      const snapshot = { workers: ctx.collab.listWorkers(), tasks: ctx.collab.listTasks(), messages: ctx.collab.listMessages() }
      return [...transports.entries()].map(([type, transport]) => ({ type, ...(typeof transport.reconcile === 'function' ? transport.reconcile(snapshot) : { keepAlive: true }), close: typeof transport.close === 'function' ? () => transport.close() : null }))
    },
  }
  for (const transport of transports.values()) {
    if (typeof transport.onEvent === 'function') transport.onEvent(({ messageId, state, event }) => {
      if (!messageId || state !== 'arrived') return
      const message = ctx.collab.listMessages().find((candidate) => candidate.messageId === messageId)
      if (message?.state === 'transport_accepted') ctx.collab.transitionMessage(messageId, state)
      void event
    })
  }
  ctx.provide('communication', hub)
  const lifecycleTimer = setInterval(() => {
    for (const result of hub.reconcileLifecycle()) {
      if (result.keepAlive) {
        idleClosed.delete(result.type)
        continue
      }
      if (idleClosed.has(result.type) || typeof result.close !== 'function') continue
      result.close()
      idleClosed.add(result.type)
    }
  }, lifecycleIntervalMs)
  lifecycleTimer.unref?.()
  return async () => {
    clearInterval(lifecycleTimer)
    await Promise.all([...transports.values()].map((transport) => typeof transport.close === 'function' ? transport.close() : undefined))
  }
}
CommunicationHub.inject = ['collab']
CommunicationHub.provide = 'communication'

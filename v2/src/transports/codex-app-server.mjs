import { createAppServerTransport } from './app-server.mjs'

export function createCodexAppServerTransport(options = {}) {
  const appServer = createAppServerTransport(options)
  const locks = new Set()
  const activeMessages = new Map()
  const activeTurns = new Map()
  const eventHandlers = new Set()
  let closed = false
  appServer.onEvent(({ address, event }) => {
    const threadId = event.params?.threadId ?? event.params?.turn?.threadId
    const key = threadId && `${address}:${threadId}`
    const active = key && activeMessages.get(key)
    if (!active) return
    if (event.method === 'turn/started' || event.method === 'item/started') {
      for (const handler of eventHandlers) handler({ messageId: active.messageId, state: 'arrived', event })
      return
    }
    if (['turn/completed', 'turn/failed', 'turn/cancelled', 'turn/aborted', 'turn/finished'].includes(event.method)) {
      activeTurns.delete(key)
      activeMessages.delete(key)
      locks.delete(key)
      for (const handler of eventHandlers) handler({ messageId: active.messageId, state: 'terminal', event })
    }
  })
  return {
    async deliver({ endpoint, payload, messageId }) {
      if (!endpoint || typeof endpoint.address !== 'string' || typeof endpoint.threadId !== 'string') throw new Error('codex app-server endpoint requires address and threadId')
      if (!payload || typeof payload.text !== 'string') throw new TypeError('codex payload requires text')
      const lockKey = `${endpoint.address}:${endpoint.threadId}`
      closed = false
      if (locks.has(lockKey)) throw new Error(`writer lock is busy: ${endpoint.threadId}`)
      locks.add(lockKey)
      activeMessages.set(lockKey, { messageId, startedAt: Date.now() })
      try {
        const result = await appServer.request(endpoint.address, 'turn/start', { threadId: endpoint.threadId, input: [{ type: 'text', text: payload.text }] })
        activeTurns.set(lockKey, { messageId, turnId: result?.turn?.id ?? result?.turnId ?? null, startedAt: Date.now() })
        return { protocol: 'codex-app-server', threadId: endpoint.threadId, turnId: result?.turn?.id ?? result?.turnId ?? null }
      } catch (error) {
        locks.delete(lockKey)
        activeMessages.delete(lockKey)
        activeTurns.delete(lockKey)
        throw error
      }
    },
    close: () => {
      if (closed) return
      closed = true
      locks.clear()
      activeMessages.clear()
      activeTurns.clear()
      appServer.close()
    },
    reconcile({ workers = [], tasks = [], messages = [] } = {}) {
      const liveWorkers = workers.some((worker) => worker.state !== 'closed' && worker.presence.status === 'online')
      const activeWork = tasks.some((task) => ['working', 'verifying', 'reviewing'].includes(task.state))
      const pendingMessages = messages.some((message) => !['completed', 'deferred', 'failed'].includes(message.state))
      return { keepAlive: liveWorkers || activeWork || pendingMessages || activeTurns.size > 0, activeTurns: activeTurns.size, clients: appServer.clientCount() }
    },
    status() {
      return { activeTurns: activeTurns.size, locks: locks.size, activeMessages: activeMessages.size }
    },
    onEvent(handler) {
      if (typeof handler !== 'function') throw new TypeError('event handler must be a function')
      eventHandlers.add(handler)
      return () => eventHandlers.delete(handler)
    },
  }
}

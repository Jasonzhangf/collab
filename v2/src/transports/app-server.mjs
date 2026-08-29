export function createAppServerTransport({ connect = (address) => new WebSocket(address), requestTimeoutMs = 30000 } = {}) {
  const clients = new Map()
  const eventHandlers = new Set()
  return {
    async request(address, method, params) {
      let client = clients.get(address)
      if (!client) {
        client = new JsonRpcClient(connect(address), (event) => {
          if (event.method === '__closed' && clients.get(address) === client) clients.delete(address)
          for (const handler of eventHandlers) handler({ address, event })
        }, requestTimeoutMs)
        await client.initialize()
        clients.set(address, client)
      }
      return client.request(method, params, requestTimeoutMs).catch((error) => {
        if (clients.get(address) === client && client.closed) clients.delete(address)
        throw error
      })
    },
    close() {
      for (const client of clients.values()) client.close()
      clients.clear()
    },
    clientCount: () => clients.size,
    onEvent(handler) {
      if (typeof handler !== 'function') throw new TypeError('event handler must be a function')
      eventHandlers.add(handler)
      return () => eventHandlers.delete(handler)
    },
  }
}

class JsonRpcClient {
  constructor(socket, onEvent, requestTimeoutMs) {
    this.socket = socket
    this.nextId = 1
    this.pending = new Map()
    this.onEvent = onEvent
    this.requestTimeoutMs = requestTimeoutMs
    this.closed = false
    socket.addEventListener('message', (event) => this.receive(event.data))
    socket.addEventListener('error', (event) => this.fail(new Error(event?.message ?? 'app-server websocket error')))
    socket.addEventListener('close', () => {
      this.closed = true
      this.fail(new Error('app-server websocket closed'))
      this.onEvent?.({ method: '__closed', params: {} })
    })
  }

  async initialize() {
    await this.ready()
    await this.request('initialize', { clientInfo: { name: 'collab-v2', title: 'Collab v2', version: '0.1.0-beta.2' } }, this.requestTimeoutMs)
    this.socket.send(JSON.stringify({ jsonrpc: '2.0', method: 'initialized', params: {} }))
  }

  ready() {
    if (this.socket.readyState === 1) return Promise.resolve()
    return new Promise((resolve, reject) => {
      this.socket.addEventListener('open', resolve, { once: true })
      this.socket.addEventListener('error', reject, { once: true })
    })
  }

  request(method, params, timeoutMs = this.requestTimeoutMs ?? 30000) {
    const id = this.nextId++
    return new Promise((resolve, reject) => {
      const timeout = setTimeout(() => {
        this.pending.delete(id)
        reject(new Error(`app-server request timeout: ${method}`))
      }, timeoutMs)
      this.pending.set(id, { resolve, reject, timeout })
      try {
        this.socket.send(JSON.stringify({ jsonrpc: '2.0', id, method, params }))
      } catch (error) {
        clearTimeout(timeout)
        this.pending.delete(id)
        reject(error)
      }
    })
  }

  receive(data) {
    const message = JSON.parse(typeof data === 'string' ? data : new TextDecoder().decode(data))
    if (message.id === undefined) {
      this.onEvent?.(message)
      return
    }
    const pending = this.pending.get(message.id)
    if (!pending) return
    this.pending.delete(message.id)
    clearTimeout(pending.timeout)
    if (message.error) pending.reject(new Error(message.error.message ?? 'app-server rpc error'))
    else pending.resolve(message.result)
  }

  fail(error) {
    for (const pending of this.pending.values()) {
      clearTimeout(pending.timeout)
      pending.reject(error)
    }
    this.pending.clear()
  }

  close() {
    if (this.closed) return
    this.closed = true
    this.fail(new Error('app-server websocket closed'))
    this.socket.close()
  }
}

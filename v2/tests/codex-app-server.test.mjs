import test from 'node:test'
import assert from 'node:assert/strict'
import { createCodexAppServerTransport } from '../src/transports/codex-app-server.mjs'

class FakeSocket {
  readyState = 0
  listeners = new Map()
  sent = []
  addEventListener(name, callback) { this.listeners.set(name, callback) }
  send(value) {
    this.sent.push(JSON.parse(value))
    const request = this.sent.at(-1)
    queueMicrotask(() => this.listeners.get('message')?.({ data: JSON.stringify({ jsonrpc: '2.0', id: request.id, result: request.method === 'turn/start' ? { turn: { id: 'turn-1' } } : {} }) }))
  }
  open() { this.readyState = 1; this.listeners.get('open')?.() }
  close() { this.readyState = 3; this.listeners.get('close')?.() }
  notify(message) { this.listeners.get('message')?.({ data: JSON.stringify(message) }) }
}

test('Codex App Server adaptor maps unified delivery to turn/start', async () => {
  const socket = new FakeSocket()
  const transport = createCodexAppServerTransport({ connect: () => { queueMicrotask(() => socket.open()); return socket } })
  const receipt = await transport.deliver({ endpoint: { address: 'ws://test', threadId: 'thread-b' }, payload: { text: 'hello' } })
  assert.deepEqual(receipt, { protocol: 'codex-app-server', threadId: 'thread-b', turnId: 'turn-1' })
  assert.equal(socket.sent.at(-1).method, 'turn/start')
  assert.equal(socket.sent.at(-1).params.threadId, 'thread-b')
  await assert.rejects(transport.deliver({ endpoint: { address: 'ws://test' }, payload: { text: 'bad' } }), /threadId/)
  transport.close()
})

test('Codex App Server adaptor rejects concurrent writer on one thread', async () => {
  let release
  const socket = new FakeSocket()
  socket.send = (value) => {
    const request = JSON.parse(value)
    socket.sent.push(request)
    if (request.method === 'turn/start') return new Promise((resolve) => { release = () => { queueMicrotask(() => socket.listeners.get('message')?.({ data: JSON.stringify({ jsonrpc: '2.0', id: request.id, result: { turn: { id: 'turn-lock' } } }) })); resolve() } })
    queueMicrotask(() => socket.listeners.get('message')?.({ data: JSON.stringify({ jsonrpc: '2.0', id: request.id, result: {} }) }))
  }
  const transport = createCodexAppServerTransport({ connect: () => { queueMicrotask(() => socket.open()); return socket } })
  const endpoint = { address: 'ws://lock', threadId: 'thread-lock' }
  const first = transport.deliver({ endpoint, payload: { text: 'first' } })
  await new Promise((resolve) => setTimeout(resolve, 0))
  await assert.rejects(transport.deliver({ endpoint, payload: { text: 'second' } }), /writer lock is busy/)
  release()
  await first
  transport.close()
})

test('Codex adaptor projects server notification to arrival without claiming acknowledgement', async () => {
  const socket = new FakeSocket()
  const originalSend = socket.send.bind(socket)
  socket.send = (value) => {
    const request = JSON.parse(value)
    if (request.method === 'turn/start') socket.notify({ jsonrpc: '2.0', method: 'turn/started', params: { threadId: request.params.threadId } })
    originalSend(value)
  }
  const transport = createCodexAppServerTransport({ connect: () => { queueMicrotask(() => socket.open()); return socket } })
  const events = []
  transport.onEvent((event) => events.push(event))
  await transport.deliver({ messageId: 'message-arrival', endpoint: { address: 'ws://arrival', threadId: 'thread-arrival' }, payload: { text: 'hello' } })
  assert.equal(events[0].messageId, 'message-arrival')
  assert.equal(events[0].state, 'arrived')
  transport.close()
})

test('Codex adaptor keeps a long-running turn locked until terminal event', async () => {
  const socket = new FakeSocket()
  const originalSend = socket.send.bind(socket)
  socket.send = (value) => {
    const request = JSON.parse(value)
    if (request.method === 'turn/start') {
      queueMicrotask(() => socket.listeners.get('message')?.({ data: JSON.stringify({ jsonrpc: '2.0', id: request.id, result: { turn: { id: 'turn-long' } } }) }))
      return
    }
    originalSend(value)
  }
  const transport = createCodexAppServerTransport({ connect: () => { queueMicrotask(() => socket.open()); return socket } })
  const endpoint = { address: 'ws://long', threadId: 'thread-long' }
  await transport.deliver({ messageId: 'message-long', endpoint, payload: { text: 'long' } })
  await assert.rejects(transport.deliver({ endpoint, payload: { text: 'second' } }), /writer lock is busy/)
  socket.notify({ jsonrpc: '2.0', method: 'turn/completed', params: { threadId: endpoint.threadId } })
  await transport.deliver({ endpoint, payload: { text: 'after terminal' } })
  transport.close()
})

test('Codex adaptor correlates arrival after turn/start response', async () => {
  const socket = new FakeSocket()
  const originalSend = socket.send.bind(socket)
  socket.send = (value) => {
    const request = JSON.parse(value)
    originalSend(value)
    if (request.method === 'turn/start') setTimeout(() => socket.notify({ jsonrpc: '2.0', method: 'turn/started', params: { threadId: request.params.threadId } }), 5)
  }
  const transport = createCodexAppServerTransport({ connect: () => { queueMicrotask(() => socket.open()); return socket } })
  const events = []
  transport.onEvent((event) => events.push(event))
  await transport.deliver({ messageId: 'message-late-arrival', endpoint: { address: 'ws://late', threadId: 'thread-late' }, payload: { text: 'hello' } })
  await new Promise((resolve) => setTimeout(resolve, 15))
  assert.equal(events[0].messageId, 'message-late-arrival')
  assert.equal(events[0].state, 'arrived')
  socket.notify({ jsonrpc: '2.0', method: 'turn/completed', params: { threadId: 'thread-late' } })
  transport.close()
})

test('app-server request timeout rejects a stalled RPC', async () => {
  const socket = new FakeSocket()
  socket.send = (value) => { socket.sent.push(JSON.parse(value)) }
  const transport = createCodexAppServerTransport({ requestTimeoutMs: 10, connect: () => { queueMicrotask(() => socket.open()); return socket } })
  await assert.rejects(transport.deliver({ endpoint: { address: 'ws://stall', threadId: 'thread-stall' }, payload: { text: 'stall' } }), /request timeout: initialize/)
  transport.close()
})

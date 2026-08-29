import { createCollabV2 } from '../src/index.mjs'

const address = process.env.COLLAB_V2_APP_SERVER ?? 'ws://127.0.0.1:8797'
const ws = new WebSocket(address)
let nextId = 0
const pending = new Map()

function call(method, params) {
  return new Promise((resolve, reject) => {
    const id = ++nextId
    pending.set(id, { resolve, reject })
    ws.send(JSON.stringify({ jsonrpc: '2.0', id, method, params }))
  })
}

ws.addEventListener('message', (event) => {
  const message = JSON.parse(event.data)
  const request = pending.get(message.id)
  if (!request) return
  pending.delete(message.id)
  message.error ? request.reject(new Error(message.error.message ?? 'RPC error')) : request.resolve(message.result)
})

await new Promise((resolve, reject) => {
  ws.addEventListener('open', resolve, { once: true })
  ws.addEventListener('error', reject, { once: true })
})
await call('initialize', { clientInfo: { name: 'collab-v2-live-smoke', title: 'collab-v2-live-smoke', version: '0.1.0-beta.1' } })
ws.send(JSON.stringify({ jsonrpc: '2.0', method: 'initialized', params: {} }))
const started = await call('thread/start', { cwd: process.cwd(), ephemeral: true })
const target = started.thread
if (!target?.id) throw new Error('thread/start did not return a writable thread')

const runtime = await createCollabV2({ cwd: process.cwd(), codexAppServer: {} })
await runtime.collab.register({ agentId: 'live-a', kind: 'codex', cwd: process.cwd(), panelId: 'live-panel-a', capabilities: ['app-server'], endpoints: [] })
await runtime.collab.register({ agentId: 'live-b', kind: 'codex', cwd: process.cwd(), panelId: 'live-panel-b', capabilities: ['app-server'], endpoints: [{ type: 'codex-app-server', address, threadId: target.id }] })
const receipt = await runtime.communication.send({ fromAgentId: 'live-a', toAgentId: 'live-b', payload: { text: 'Reply exactly COLLAB-V2-LIVE-2.' } })
console.log(JSON.stringify({ target: { id: target.id, cwd: target.cwd }, receipt, messageState: runtime.collab.listMessages()[0].state }))
await runtime.dispose()
ws.close()

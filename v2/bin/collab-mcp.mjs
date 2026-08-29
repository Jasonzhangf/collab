#!/usr/bin/env node
import { createInterface } from 'node:readline'
import { existsSync, mkdirSync, readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { createCollabV2 } from '../src/index.mjs'
import { createFilePersistence } from '../src/persistence.mjs'
import { createTmuxTransport } from '../src/transports/tmux.mjs'

const cwd = resolve(process.cwd())
mkdirSync(resolve(cwd, '.agent-collab-v2'), { recursive: true })
const statePath = resolve(cwd, '.agent-collab-v2/state.json')
const runtime = await createCollabV2({ cwd, persistence: createFilePersistence(statePath), transports: { tmux: createTmuxTransport() } })

const tool = (name, description, properties = {}, required = []) => ({ name, description, inputSchema: { type: 'object', properties, required, additionalProperties: false } })
const tools = [
  tool('collab_register', 'Register this agent with its self-reported identity and capabilities.', { worker: { type: 'object' }, threadId: { type: 'string' } }, ['worker']),
  tool('collab_whoami', 'Return this panel identity and the current project snapshot.', { panelId: { type: 'string' } }),
  tool('collab_workers', 'List registered workers and presence.'),
  tool('collab_master', 'Return the current master worker.'),
  tool('collab_tasks', 'List project tasks.'),
  tool('collab_task_create', 'Create a task; master permission required.', { taskId: { type: 'string' }, title: { type: 'string' }, actorAgentId: { type: 'string' } }, ['taskId', 'title', 'actorAgentId']),
  tool('collab_task_claim', 'Claim an available task.', { taskId: { type: 'string' }, agentId: { type: 'string' } }, ['taskId', 'agentId']),
  tool('collab_task_transition', 'Advance a task through its lifecycle.', { taskId: { type: 'string' }, state: { type: 'string' }, actorAgentId: { type: 'string' } }, ['taskId', 'state']),
  tool('collab_send', 'Send a business message through the target registered transport.', { fromAgentId: { type: 'string' }, toAgentId: { type: 'string' }, payload: { type: 'object' } }, ['fromAgentId', 'toAgentId', 'payload']),
  tool('collab_presence', 'Update this agent presence.', { agentId: { type: 'string' }, state: { type: 'string', enum: ['heartbeat', 'offline'] } }, ['agentId', 'state']),
  tool('collab_test', 'Run the registered agent lifecycle and transport self-check.', { panelId: { type: 'string' } }),
]

function result(value) { return { content: [{ type: 'text', text: JSON.stringify(value) }] } }
function panelId(args) { return args.panelId ?? process.env.TMUX_PANE }
async function call(name, args = {}) {
  switch (name) {
    case 'collab_register': {
      const worker = structuredClone(args.worker)
      const threadId = args.threadId ?? process.env.CODEX_THREAD_ID
      const appServerPath = resolve(cwd, '.agent-collab-v2/app-server.json')
      if (worker.capabilities?.includes('app-server') && (!worker.endpoints || worker.endpoints.length === 0) && threadId && existsSync(appServerPath)) {
        const appServer = JSON.parse(readFileSync(appServerPath, 'utf8'))
        worker.endpoints = [{ type: 'codex-app-server', address: appServer.address, threadId }]
      }
      return runtime.collab.register(worker)
    }
    case 'collab_whoami': return { identity: runtime.collab.whoami(panelId(args)), snapshot: runtime.dashboard.snapshot() }
    case 'collab_workers': return runtime.collab.listWorkers()
    case 'collab_master': return runtime.collab.listWorkers().find((worker) => worker.role === 'master') ?? null
    case 'collab_tasks': return runtime.collab.listTasks()
    case 'collab_task_create': return runtime.collab.createTask(args)
    case 'collab_task_claim': return runtime.collab.claimTask(args.taskId, args.agentId)
    case 'collab_task_transition': return runtime.collab.transitionTask(args.taskId, args.state, args.actorAgentId)
    case 'collab_send': return runtime.communication.send(args)
    case 'collab_presence': return args.state === 'heartbeat' ? runtime.collab.heartbeat(args.agentId) : runtime.collab.markOffline(args.agentId)
    case 'collab_test': {
      const identity = runtime.collab.whoami(panelId(args))
      return { identity, lifecycle: runtime.communication.reconcileLifecycle(), snapshot: runtime.dashboard.snapshot() }
    }
    default: throw new Error(`unknown tool: ${name}`)
  }
}

const rl = createInterface({ input: process.stdin, crlfDelay: Infinity })
let requestChain = Promise.resolve()
let outputChain = Promise.resolve()
function writeResponse(response) {
  outputChain = outputChain.then(() => new Promise((resolve) => process.stdout.write(`${JSON.stringify(response)}\n`, resolve)))
  return outputChain
}
rl.on('line', (line) => {
  requestChain = requestChain.then(async () => {
  if (!line.trim()) return
  let request
  try { request = JSON.parse(line) } catch { return }
  if (!request.id) return
  try {
    let response
    if (request.method === 'initialize') response = { protocolVersion: '2024-11-05', capabilities: { tools: {} }, serverInfo: { name: 'collab', version: '0.1.0-beta.2' } }
    else if (request.method === 'tools/list') response = { tools }
    else if (request.method === 'tools/call') response = result(await call(request.params?.name, request.params?.arguments ?? {}))
    else throw new Error(`method not found: ${request.method}`)
    await writeResponse({ jsonrpc: '2.0', id: request.id, result: response })
  } catch (error) {
    await writeResponse({ jsonrpc: '2.0', id: request.id, error: { code: -32000, message: error.message } })
  }
  }).catch(() => {})
})
process.on('SIGTERM', async () => { await runtime.dispose(); process.exit(0) })
process.on('SIGINT', async () => { await runtime.dispose(); process.exit(0) })

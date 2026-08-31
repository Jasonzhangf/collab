#!/usr/bin/env node
import { createInterface } from 'node:readline'
import { mkdirSync } from 'node:fs'
import { resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { createCollabV2 } from '../src/index.mjs'
import { resolveProjectEnvironment } from '../src/project-environment.mjs'

const { projectRoot, pane, sessionId } = resolveProjectEnvironment()
const controlDir = resolve(projectRoot, '.agent-collab-v2')
mkdirSync(controlDir, { recursive: true })
const runtime = await createCollabV2({
  cwd: projectRoot,
  rustCoreBinary: process.env.COLLAB_V2_CORE_BINARY ?? fileURLToPath(new URL('../generated/modules/core/lib/core-daemon', import.meta.url)),
  rustCoreState: resolve(controlDir, 'state.json'),
})
const tool = (name, description, properties = {}, required = []) => ({ name, description, inputSchema: { type: 'object', properties, required, additionalProperties: false } })
const tools = [
  tool('collab_register', 'Register the inherited tmux session as an equal peer.'),
  tool('collab_context', 'Read this peer identity, owned tasks, messages, and subscriptions.'),
  tool('collab_task_register', 'Register this peer-owned task directly as working.', { taskId: { type: 'string' }, featureId: { type: 'string' }, resourceId: { type: 'string' } }, ['taskId', 'featureId', 'resourceId']),
  tool('collab_task_update', 'Apply an owner-only adjacent task transition.', { taskId: { type: 'string' }, state: { type: 'string', enum: ['working', 'blocked', 'verifying', 'reviewed', 'delivered', 'rework', 'merged', 'closed', 'cancelled'] } }, ['taskId', 'state']),
  tool('collab_task_wait', 'Create a finite resource-conflict wait edge.', { taskId: { type: 'string' }, blockingTaskId: { type: 'string' }, deadlineMs: { type: 'integer' } }, ['taskId', 'blockingTaskId', 'deadlineMs']),
  tool('collab_send_resource_notice', 'Commit RESOURCE_OCCUPIED or RESOURCE_RELEASED durable notice.', { messageId: { type: 'string' }, to: { type: 'string' }, notice: { type: 'string', enum: ['occupied', 'released'] }, subject: { type: 'string' } }, ['messageId', 'to', 'notice', 'subject']),
  tool('collab_notify_methods', 'List supported opt-in live notification methods.'),
  tool('collab_notify_subscribe', 'Register an exact finite one-shot wake subscription.', { subscriptionId: { type: 'string' }, event: { type: 'string', enum: ['direct-message', 'resource-released', 'deadline', 'async-result'] }, subject: { type: 'string' }, ttlMs: { type: 'integer' } }, ['subscriptionId', 'event', 'ttlMs']),
  tool('collab_notify_status', 'Read this peer notification subscriptions.'),
  tool('collab_notify_unsubscribe', 'Cancel this peer owned armed subscription.', { subscriptionId: { type: 'string' } }, ['subscriptionId']),
  tool('collab_migrate_inspect', 'Inspect fixed role-based v2 beta state without mutation.'),
  tool('collab_migrate_plan', 'Build a deterministic role-free migration plan or return blocking issues.'),
  tool('collab_migrate_apply', 'Freeze mutations and apply the deterministic migration plan.'),
  tool('collab_migrate_verify', 'Verify rebound identity and exact migration continuity while frozen.'),
  tool('collab_migrate_resume', 'Resume mutations after migration verification.'),
]
function currentIdentity() {
  if (!sessionId) return null
  return runtime.collab.snapshot().identities.find((identity) => identity.session_id === sessionId) ?? null
}
function requireIdentity() {
  const identity = currentIdentity()
  if (!identity) throw new Error('registration required')
  return identity
}
function context() {
  const identity = currentIdentity()
  const state = runtime.collab.snapshot()
  return { identity, registration: identity ? 'registered' : 'required', tasks: identity ? state.tasks.filter((task) => task.owner === identity.id) : [], messages: identity ? state.messages.filter((message) => message.from === identity.id || message.to === identity.id) : [], subscriptions: identity ? state.subscriptions.filter((subscription) => subscription.owner === identity.id) : [] }
}
async function call(name, args = {}) {
  if (name === 'collab_register') {
    if (!pane || !sessionId) throw new Error('registration requires an inherited tmux pane')
    runtime.collab.register({ id: sessionId, sessionId, pane })
    return context()
  }
  if (name === 'collab_context') return context()
  const identity = requireIdentity()
  if (name === 'collab_task_register') return runtime.collab.registerTask({ actor: identity.id, ...args })
  if (name === 'collab_task_update') return runtime.collab.transitionTask({ actor: identity.id, taskId: args.taskId, state: args.state })
  if (name === 'collab_task_wait') return runtime.collab.waitTask({ actor: identity.id, taskId: args.taskId, blockingTaskId: args.blockingTaskId, deadlineMs: args.deadlineMs, nowMs: Date.now() })
  if (name === 'collab_send_resource_notice') return runtime.communication.send({ messageId: args.messageId, from: identity.id, to: args.to, notice: args.notice, subject: args.subject })
  if (name === 'collab_notify_methods') return { methods: [{ method: 'tmux', live: true, opt_in: true, payload: 'COLLAB_NOTIFY <message-id>' }] }
  if (name === 'collab_notify_subscribe') return runtime.collab.subscribe({ owner: identity.id, subscriptionId: args.subscriptionId, event: args.event, subject: args.subject ?? null, expiresAtMs: Date.now() + args.ttlMs, nowMs: Date.now() })
  if (name === 'collab_notify_status') return runtime.collab.snapshot().subscriptions.filter((subscription) => subscription.owner === identity.id)
  if (name === 'collab_notify_unsubscribe') return runtime.collab.unsubscribe({ owner: identity.id, subscriptionId: args.subscriptionId })
  if (name === 'collab_migrate_inspect') return runtime.collab.migrationInspect()
  if (name === 'collab_migrate_plan') return runtime.collab.migrationPlan()
  if (name === 'collab_migrate_apply') return runtime.collab.migrationApply()
  if (name === 'collab_migrate_verify') return runtime.collab.migrationVerify()
  if (name === 'collab_migrate_resume') return runtime.collab.migrationResume()
  throw new Error(`unknown tool: ${name}`)
}
const result = (value) => ({ content: [{ type: 'text', text: JSON.stringify(value) }] })
const rl = createInterface({ input: process.stdin, crlfDelay: Infinity })
let chain = Promise.resolve()
rl.on('line', (line) => {
  chain = chain.then(async () => {
    if (!line.trim()) return
    const request = JSON.parse(line)
    if (request.id === undefined) return
    try {
      let response
      if (request.method === 'initialize') response = { protocolVersion: '2024-11-05', capabilities: { tools: {} }, serverInfo: { name: 'collab', version: '0.1.0-beta.2' } }
      else if (request.method === 'tools/list') response = { tools }
      else if (request.method === 'tools/call') response = result(await call(request.params?.name, request.params?.arguments ?? {}))
      else throw new Error(`method not found: ${request.method}`)
      process.stdout.write(`${JSON.stringify({ jsonrpc: '2.0', id: request.id, result: response })}\n`)
    } catch (error) {
      process.stdout.write(`${JSON.stringify({ jsonrpc: '2.0', id: request.id, error: { code: -32000, message: error.message } })}\n`)
    }
  }).catch((error) => process.stderr.write(`${error.stack ?? error.message}\n`))
})
for (const signal of ['SIGTERM', 'SIGINT']) process.on(signal, async () => { await runtime.dispose(); process.exit(0) })

import test from 'node:test'
import assert from 'node:assert/strict'
import { createCollabV2 } from '../src/index.mjs'
import { createMailboxTransport } from '../src/transports/mailbox.mjs'
import { mkdtemp } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

const worker = (agentId, panelId, capabilities = []) => ({
  agentId, panelId, kind: agentId.startsWith('codex') ? 'codex' : 'generic',
  cwd: '/tmp/project', capabilities, endpoints: [],
})

test('first verified registration is master and later registration is worker', async () => {
  const seen = []
  const runtime = await createCollabV2({ cwd: '/tmp/project', verifyCapability: async ({ agentId, capability }) => {
    seen.push(`${agentId}:${capability}`)
    return capability !== 'broken'
  } })
  const master = await runtime.collab.register(worker('codex-a', 'panel-a', ['app-server']))
  const member = await runtime.collab.register(worker('worker-b', 'panel-b', [{ id: 'tmux' }]))
  assert.equal(master.role, 'master')
  assert.equal(member.role, 'worker')
  assert.deepEqual(seen, ['codex-a:app-server', 'worker-b:tmux'])
  await runtime.dispose()
})

test('registration rejects panel identity reuse', async () => {
  const runtime = await createCollabV2({ cwd: '/tmp/project' })
  await runtime.collab.register(worker('codex-a', 'panel-a'))
  await assert.rejects(() => runtime.collab.register(worker('codex-b', 'panel-a')), /panel already registered/)
  await runtime.dispose()
})

test('registration rejects unverified capability and cwd mismatch', async () => {
  const runtime = await createCollabV2({ cwd: '/tmp/project', verifyCapability: async ({ capability }) => capability !== 'broken' })
  await assert.rejects(runtime.collab.register(worker('bad', 'panel-bad', ['broken'])), /capability verification failed/)
  await assert.rejects(runtime.collab.register({ ...worker('wrong-cwd', 'panel-wrong'), cwd: '/tmp/other' }), /cwd mismatch/)
  assert.deepEqual(runtime.collab.listWorkers(), [])
  await runtime.dispose()
})

test('current panel identity can be transferred explicitly', async () => {
  const runtime = await createCollabV2({ cwd: '/tmp/project' })
  await runtime.collab.register(worker('codex-a', 'panel-a'))
  assert.equal(runtime.collab.whoami('panel-a').agentId, 'codex-a')
  runtime.collab.transferIdentity('panel-a', 'panel-a-new', 'codex-a')
  assert.throws(() => runtime.collab.whoami('panel-a'), /unknown panel/)
  assert.equal(runtime.collab.whoami('panel-a-new').agentId, 'codex-a')
  await runtime.dispose()
})

test('task claim lifecycle is explicit and transport is selected by registered endpoint', async () => {
  const delivered = []
  const runtime = await createCollabV2({
    cwd: '/tmp/project',
    transports: { 'fake-channel': { deliver: async (message) => { delivered.push(message); return { ok: true } } } },
  })
  await runtime.collab.register({ ...worker('codex-a', 'panel-a'), endpoints: [{ type: 'fake-channel', address: 'test://a' }] })
  await runtime.collab.register({ ...worker('codex-b', 'panel-b'), endpoints: [{ type: 'fake-channel', address: 'test://b' }] })
  const task = runtime.collab.createTask({ taskId: 'task-1', title: 'exercise lifecycle', actorAgentId: 'codex-a' })
  assert.equal(task.state, 'available')
  assert.equal(runtime.collab.claimTask('task-1', 'codex-a').state, 'working')
  assert.equal(runtime.collab.transitionTask('task-1', 'verifying').state, 'verifying')
  const receipt = await runtime.communication.send({ fromAgentId: 'codex-a', toAgentId: 'codex-b', payload: { kind: 'notice', text: 'ready' } })
  assert.equal(receipt.transport, 'fake-channel')
  assert.equal(delivered[0].payload.text, 'ready')
  assert.equal(runtime.collab.listMessages()[0].state, 'transport_accepted')
  await runtime.dispose()
})

test('presence, message and project closeout remain explicit', async () => {
  const runtime = await createCollabV2({ cwd: '/tmp/project' })
  await runtime.collab.register(worker('codex-a', 'panel-a'))
  await runtime.collab.register(worker('codex-b', 'panel-b'))
  assert.equal(runtime.collab.whoami('panel-a').presence.status, 'online')
  assert.equal(runtime.collab.markOffline('codex-b').status, 'offline')
  assert.equal(runtime.collab.heartbeat('codex-b').status, 'online')
  const task = runtime.collab.createTask({ taskId: 'close-task', title: 'closeout', actorAgentId: 'codex-a' })
  runtime.collab.claimTask(task.taskId, 'codex-a')
  runtime.collab.transitionTask(task.taskId, 'verifying')
  runtime.collab.transitionTask(task.taskId, 'reviewing')
  runtime.collab.transitionTask(task.taskId, 'delivered')
  runtime.collab.transitionTask(task.taskId, 'merged')
  runtime.collab.transitionTask(task.taskId, 'closed')
  const message = runtime.collab.createMessage({ messageId: 'message-1', fromAgentId: 'codex-a', toAgentId: 'codex-b', payload: { text: 'done' } })
  runtime.collab.transitionMessage(message.messageId, 'policy_checked')
  runtime.collab.transitionMessage(message.messageId, 'queued')
  runtime.collab.transitionMessage(message.messageId, 'deferred')
  assert.equal(runtime.collab.drain(), 'draining')
  assert.equal(runtime.collab.closeProject('codex-a'), 'closed')
  await runtime.dispose()
})

test('communication dispose closes registered transports', async () => {
  let closed = 0
  const runtime = await createCollabV2({ cwd: '/tmp/project', transports: { fake: { deliver: async () => ({ ok: true }), close: async () => { closed += 1 } } } })
  await runtime.dispose()
  assert.equal(closed, 1)
})

test('workspace lifecycle keeps app-server transport for workers and closes it when idle', async () => {
  let closed = 0
  const runtime = await createCollabV2({ cwd: '/tmp/project', lifecycleIntervalMs: 5, transports: {
    'codex-app-server': {
      deliver: async () => ({ ok: true }),
      reconcile: ({ workers, tasks, messages }) => ({ keepAlive: workers.some((worker) => worker.state !== 'closed') || tasks.some((task) => task.state === 'working') || messages.some((message) => !['completed', 'deferred', 'failed'].includes(message.state)) }),
      close: () => { closed += 1 },
    },
  } })
  await runtime.collab.register(worker('codex-a', 'panel-a', ['app-server']))
  assert.equal(runtime.communication.reconcileLifecycle()[0].keepAlive, true)
  runtime.collab.close('codex-a')
  await new Promise((resolve) => setTimeout(resolve, 15))
  assert.equal(closed, 1)
  await runtime.dispose()
})

test('dashboard is a read-only projection and AppSDK integration records scope', async () => {
  const records = []
  const runtime = await createCollabV2({ cwd: '/tmp/project', appsdkIntegration: { onRecord: (record) => records.push(record) } })
  await runtime.collab.register(worker('codex-a', 'panel-a'))
  const record = runtime.appsdkIntegration.record('worker_registered', { agentId: 'codex-a' })
  const snapshot = runtime.dashboard.snapshot()
  assert.equal(record.scope, '/tmp/project')
  assert.equal(records.length, 1)
  assert.equal(snapshot.workers[0].agentId, 'codex-a')
  assert.equal(snapshot.projectState, 'accepting_workers')
  await runtime.dispose()
})

test('standalone and AppSDK composition expose the same collab services', async () => {
  const runtime = await createCollabV2({ cwd: '/tmp/project' })
  assert.equal(typeof runtime.ctx.collab.register, 'function')
  assert.equal(typeof runtime.ctx.dashboard.snapshot, 'function')
  assert.equal(typeof runtime.ctx.appsdkIntegration.record, 'function')
  await runtime.dispose()
})

test('worker cannot perform master-only governance operations', async () => {
  const runtime = await createCollabV2({ cwd: '/tmp/project' })
  await runtime.collab.register(worker('codex-a', 'panel-a'))
  await runtime.collab.register(worker('codex-b', 'panel-b'))
  assert.throws(() => runtime.collab.createTask({ taskId: 'forbidden', title: 'nope', actorAgentId: 'codex-b' }), /permission denied: assign/)
  assert.throws(() => runtime.collab.transferIdentity('panel-a', 'panel-new', 'codex-b'), /permission denied: transfer/)
  assert.throws(() => runtime.collab.closeProject('codex-b'), /permission denied: close-project/)
  await runtime.dispose()
})

test('offline target is deferred without transport acceptance or wake claim', async () => {
  const delivered = []
  const runtime = await createCollabV2({ cwd: '/tmp/project', transports: { fake: { deliver: async (message) => delivered.push(message) } } })
  await runtime.collab.register(worker('a', 'pa'))
  await runtime.collab.register({ ...worker('b', 'pb'), endpoints: [{ type: 'fake', address: 'offline' }] })
  runtime.collab.markOffline('b')
  const receipt = await runtime.communication.send({ fromAgentId: 'a', toAgentId: 'b', payload: { text: 'later' } })
  assert.equal(receipt.status, 'deferred')
  assert.equal(delivered.length, 0)
  assert.equal(runtime.collab.listMessages()[0].state, 'deferred')
  await runtime.dispose()
})

test('mailbox receive, acknowledgement and completion close the message lifecycle', async () => {
  const root = await mkdtemp(join(tmpdir(), 'collab-v2-message-'))
  const runtime = await createCollabV2({ cwd: '/tmp/project', transports: { mailbox: createMailboxTransport({ root }) } })
  await runtime.collab.register(worker('a', 'pa'))
  await runtime.collab.register({ ...worker('b', 'pb'), endpoints: [{ type: 'mailbox', mailboxId: 'b' }] })
  const delivery = await runtime.communication.send({ fromAgentId: 'a', toAgentId: 'b', payload: { text: 'complete' } })
  const endpoint = { type: 'mailbox', mailboxId: 'b' }
  assert.equal((await runtime.communication.receive({ agentId: 'b', endpoint })).length, 1)
  assert.equal(runtime.collab.listMessages()[0].state, 'arrived')
  await runtime.communication.acknowledge({ messageId: delivery.messageId, agentId: 'b' })
  assert.equal(runtime.collab.listMessages()[0].state, 'acknowledged')
  assert.equal(runtime.collab.transitionMessage(delivery.messageId, 'completed').state, 'completed')
  await runtime.dispose()
})

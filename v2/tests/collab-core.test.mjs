import test from 'node:test'
import assert from 'node:assert/strict'
import { mkdtemp } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { createCollabV2 } from '../src/index.mjs'

const binary = resolve('target/debug/core-daemon')

async function runtime() {
  const root = await mkdtemp(join(tmpdir(), 'collab-v2-core-'))
  return createCollabV2({ cwd: '/tmp/project', rustCoreBinary: binary, rustCoreState: join(root, 'state.json') })
}

test('Node adapter registers equal peers without role or permission projection', async () => {
  const instance = await runtime()
  instance.collab.register({ id: 'a', sessionId: 'session-a', pane: '%1' })
  instance.collab.register({ id: 'b', sessionId: 'session-b', pane: '%2' })
  const snapshot = instance.collab.snapshot()
  assert.deepEqual(snapshot.identities.map(({ id, kind }) => ({ id, kind })), [{ id: 'a', kind: 'peer' }, { id: 'b', kind: 'peer' }])
  assert.equal('role' in snapshot.identities[0], false)
  assert.equal('permissions' in snapshot.identities[0], false)
  await instance.dispose()
})

test('task owner self-registers working lifecycle and another peer cannot mutate it', async () => {
  const instance = await runtime()
  instance.collab.register({ id: 'a', sessionId: 'session-a', pane: '%1' })
  instance.collab.register({ id: 'b', sessionId: 'session-b', pane: '%2' })
  instance.collab.registerTask({ actor: 'a', taskId: 'task-1', featureId: 'feature', resourceId: 'resource' })
  assert.equal(instance.collab.snapshot().tasks[0].state, 'working')
  assert.throws(() => instance.collab.transitionTask({ actor: 'b', taskId: 'task-1', state: 'cancelled' }), /PermissionDenied/)
  for (const state of ['verifying', 'reviewed', 'delivered', 'merged', 'closed']) instance.collab.transitionTask({ actor: 'a', taskId: 'task-1', state })
  assert.equal(instance.collab.snapshot().tasks[0].state, 'closed')
  assert.equal(typeof instance.collab.claimTask, 'undefined')
  assert.equal(typeof instance.collab.heartbeat, 'undefined')
  await instance.dispose()
})

test('restart projection comes from persisted Rust state rather than Node maps', async () => {
  const root = await mkdtemp(join(tmpdir(), 'collab-v2-restart-'))
  const state = join(root, 'state.json')
  const first = await createCollabV2({ cwd: '/tmp/project', rustCoreBinary: binary, rustCoreState: state })
  first.collab.register({ id: 'a', sessionId: 'session-a', pane: '%1' })
  first.collab.registerTask({ actor: 'a', taskId: 'task-1', featureId: 'feature', resourceId: 'resource' })
  await first.dispose()
  const second = await createCollabV2({ cwd: '/tmp/project', rustCoreBinary: binary, rustCoreState: state })
  assert.equal(second.collab.snapshot().identities[0].id, 'a')
  assert.equal(second.collab.snapshot().tasks[0].id, 'task-1')
  await second.dispose()
})

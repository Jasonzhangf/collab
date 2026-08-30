import test from 'node:test'
import assert from 'node:assert/strict'
import { spawn } from 'node:child_process'
import { once } from 'node:events'
import { resolve } from 'node:path'

const binary = resolve('target/debug/core-daemon')

function client() {
  const child = spawn(binary, [], { stdio: ['pipe', 'pipe', 'inherit'] })
  const pending = []
  let buffer = ''
  child.stdout.on('data', (chunk) => {
    buffer += chunk
    while (buffer.includes('\n')) {
      const index = buffer.indexOf('\n')
      pending.shift()?.(JSON.parse(buffer.slice(0, index)))
      buffer = buffer.slice(index + 1)
    }
  })
  return { child, send(command) { return new Promise((resolveResponse) => { pending.push(resolveResponse); child.stdin.write(`${JSON.stringify(command)}\n`) }) } }
}

test('Rust core stdio protocol exposes equal-peer owner lifecycle', async () => {
  const process = client()
  assert.deepEqual(await process.send({ op: 'register', identity: { id: 'a', session_id: 'session-1', pane: '%1' } }), { ok: true })
  assert.deepEqual(await process.send({ op: 'register', identity: { id: 'b', session_id: 'session-2', pane: '%2' } }), { ok: true })
  assert.deepEqual(await process.send({ op: 'register_task', actor: 'a', task_id: 'task-1', feature_id: 'feature', resource_id: 'resource' }), { ok: true })
  assert.deepEqual(await process.send({ op: 'transition_task', actor: 'b', task_id: 'task-1', state: 'cancelled' }), { ok: false, error: 'PermissionDenied' })
  for (const state of ['verifying', 'reviewed', 'delivered', 'merged', 'closed']) assert.deepEqual(await process.send({ op: 'transition_task', actor: 'a', task_id: 'task-1', state }), { ok: true })
  const snapshot = await process.send({ op: 'snapshot' })
  assert.equal(snapshot.state.identities[0].kind, 'peer')
  assert.equal(snapshot.state.tasks[0].state, 'closed')
  assert.equal('role' in snapshot.state.identities[0], false)
  process.child.stdin.end()
  await once(process.child, 'exit')
})

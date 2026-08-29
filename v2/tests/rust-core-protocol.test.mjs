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

test('Rust core stdio protocol enforces lifecycle and role permissions', async () => {
  const process = client()
  const invalid = await process.send({ op: 'unknown' })
  assert.equal(invalid.ok, false)
  assert.equal(invalid.error, 'InvalidCommand')
  assert.match(invalid.message, /unknown variant `unknown`/)
  assert.deepEqual(await process.send({ op: 'register', identity: { id: 'm', session_id: 'session-1', role: 'Master' } }), { ok: true })
  assert.deepEqual(await process.send({ op: 'register', identity: { id: 'w', session_id: 'session-2', role: 'Worker' } }), { ok: true })
  assert.deepEqual(await process.send({ op: 'create_task', actor: 'w', task_id: 'bad' }), { ok: false, error: 'PermissionDenied' })
  assert.deepEqual(await process.send({ op: 'create_task', actor: 'm', task_id: 'task-1' }), { ok: true })
  assert.deepEqual(await process.send({ op: 'claim', actor: 'w', task_id: 'task-1' }), { ok: true })
  assert.deepEqual(await process.send({ op: 'transition', actor: 'm', task_id: 'task-1', state: 'Delivered' }), { ok: false, error: 'InvalidTransition' })
  for (const state of ['Verifying', 'Reviewing', 'Delivered', 'Merged', 'Closed']) assert.deepEqual(await process.send({ op: 'transition', actor: 'w', task_id: 'task-1', state }), { ok: true })
  const snapshot = await process.send({ op: 'snapshot' })
  assert.equal(snapshot.state.tasks[0].state, 'Closed')
  process.child.stdin.end()
  await once(process.child, 'exit')
})

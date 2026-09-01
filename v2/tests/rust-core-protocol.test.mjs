import test from 'node:test'
import assert from 'node:assert/strict'
import { spawn, spawnSync } from 'node:child_process'
import { once } from 'node:events'
import { mkdtemp, readFile, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'

const binary = resolve('target/debug/core-daemon')

function client(args = []) {
  const child = spawn(binary, args, { stdio: ['pipe', 'pipe', 'inherit'] })
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
  assert.equal((await process.send({ op: 'register', command_id: 'register-a', identity: { id: 'a', session_id: 'session-1', pane: '%1' } })).ok, true)
  assert.equal((await process.send({ op: 'register', command_id: 'register-b', identity: { id: 'b', session_id: 'session-2', pane: '%2' } })).ok, true)
  assert.equal((await process.send({ op: 'register_task', command_id: 'task-register', actor: 'a', task_id: 'task-1', feature_id: 'feature', resource_id: 'resource' })).ok, true)
  assert.deepEqual(await process.send({ op: 'transition_task', command_id: 'task-denied', actor: 'b', task_id: 'task-1', state: 'cancelled' }), { ok: false, error: 'PermissionDenied' })
  let transition = 0
  for (const state of ['verifying', 'reviewed', 'delivered', 'merged', 'closed']) assert.equal((await process.send({ op: 'transition_task', command_id: `transition-${++transition}`, actor: 'a', task_id: 'task-1', state })).ok, true)
  const snapshot = await process.send({ op: 'snapshot' })
  assert.equal(snapshot.state.identities[0].kind, 'peer')
  assert.equal(snapshot.state.tasks[0].state, 'closed')
  assert.equal('role' in snapshot.state.identities[0], false)
  process.child.stdin.end()
  await once(process.child, 'exit')
})

test('journal replay is idempotent and duplicate writer fails closed', async () => {
  const root = await mkdtemp(join(tmpdir(), 'collab-v2-journal-'))
  const statePath = join(root, 'state.json')
  const process = client(['--state', statePath])
  const command = { op: 'register', command_id: 'command-1', identity: { id: 'a', session_id: 'session-a', pane: '%1' } }
  assert.equal((await process.send(command)).sequence, 1)
  assert.deepEqual(await process.send(command), { ok: true, idempotent: true, sequence: 1 })
  const duplicate = spawnSync(binary, ['--state', statePath], { input: '{"op":"snapshot"}\n', encoding: 'utf8' })
  assert.equal(duplicate.status, 2)
  assert.match(duplicate.stderr, /DuplicateWriter/)
  process.child.stdin.end()
  await once(process.child, 'exit')
  const replay = spawnSync(binary, ['--state', statePath], { input: '{"op":"snapshot"}\n', encoding: 'utf8' })
  assert.equal(replay.status, 0, replay.stderr)
  const snapshot = JSON.parse(replay.stdout)
  assert.equal(snapshot.state.sequence, 1)
  assert.match(snapshot.snapshot_sha256, /^[a-f0-9]{64}$/)
  assert.equal((await readFile(join(root, 'journal.jsonl'), 'utf8')).trim().split('\n').length, 1)
})

test('journal gap fails replay without rewriting truth', async () => {
  const root = await mkdtemp(join(tmpdir(), 'collab-v2-journal-gap-'))
  const statePath = join(root, 'state.json')
  const entry = { sequence: 2, command_id: 'command-2', command: { op: 'register', identity: { id: 'a', session_id: 'session-a', pane: '%1', kind: 'peer' } } }
  await writeFile(join(root, 'journal.jsonl'), `${JSON.stringify(entry)}\n`)
  const replay = spawnSync(binary, ['--state', statePath], { input: '{"op":"snapshot"}\n', encoding: 'utf8' })
  assert.equal(replay.status, 2)
  assert.match(replay.stderr, /ReplayFailed: JournalGap/)
  assert.equal(await readFile(join(root, 'journal.jsonl'), 'utf8'), `${JSON.stringify(entry)}\n`)
})

test('invalid message id is a typed error with zero journal or state mutation', async () => {
  const root = await mkdtemp(join(tmpdir(), 'collab-v2-message-id-'))
  const statePath = join(root, 'state.json')
  const process = client(['--state', statePath])
  assert.equal((await process.send({ op: 'register', command_id: 'register-a', identity: { id: 'a', session_id: 'session-a', pane: '%1' } })).ok, true)
  assert.equal((await process.send({ op: 'register', command_id: 'register-b', identity: { id: 'b', session_id: 'session-b', pane: '%2' } })).ok, true)
  const before = await process.send({ op: 'snapshot' })
  const response = await process.send({ op: 'send_resource_notice', command_id: 'invalid-message', message_id: 'bad\nSECOND_COMMAND', from: 'a', to: 'b', notice: 'occupied', subject: 'resource' })
  const after = await process.send({ op: 'snapshot' })
  process.child.stdin.end()
  await once(process.child, 'exit')

  assert.deepEqual(response, { ok: false, error: 'InvalidMessageId' })
  assert.deepEqual(after, before)
  assert.equal((await readFile(join(root, 'journal.jsonl'), 'utf8')).trim().split('\n').length, 2)
})

test('role-based beta migration inspects freezes rebinds verifies and resumes', async () => {
  const root = await mkdtemp(join(tmpdir(), 'collab-v2-migration-'))
  const statePath = join(root, 'state.json')
  const legacy = '{"identities":[{"id":"a","session_id":"session-a","role":"Master"}],"tasks":[{"id":"task","state":"Reviewing","owner":"a"}]}'
  await writeFile(join(root, 'legacy-state.json'), legacy)
  const invoke = (command) => {
    const result = spawnSync(binary, ['--state', statePath], { input: `${JSON.stringify(command)}\n`, encoding: 'utf8' })
    assert.equal(result.status, 0, result.stderr)
    return JSON.parse(result.stdout)
  }
  const inspection = invoke({ op: 'migration_inspect' })
  assert.equal(inspection.ok, true)
  assert.equal(JSON.stringify(inspection).includes('Master'), false)
  assert.deepEqual(inspection.plan.issues, [])
  assert.equal(invoke({ op: 'migration_apply', command_id: 'migration-apply' }).ok, true)
  assert.equal(invoke({ op: 'transition_task', command_id: 'blocked-transition', actor: 'a', task_id: 'task', state: 'delivered' }).error, 'InvalidMigrationState')
  assert.equal(invoke({ op: 'register', command_id: 'identity-rebind', identity: { id: 'a', session_id: 'session-a', pane: '%1' } }).ok, true)
  assert.equal(invoke({ op: 'migration_verify', command_id: 'migration-verify' }).ok, true)
  assert.equal(invoke({ op: 'migration_resume', command_id: 'migration-resume' }).ok, true)
  assert.equal(invoke({ op: 'transition_task', command_id: 'delivered-transition', actor: 'a', task_id: 'task', state: 'delivered' }).ok, true)
  const snapshot = invoke({ op: 'snapshot' })
  assert.equal(snapshot.state.migration.phase, 'resumed')
  assert.equal(snapshot.state.identities[0].pane, '%1')
  assert.equal(snapshot.state.tasks[0].state, 'delivered')
  assert.equal(await readFile(join(root, 'legacy-state.json'), 'utf8'), legacy)
})

test('ambiguous available beta task blocks plan without creating journal', async () => {
  const root = await mkdtemp(join(tmpdir(), 'collab-v2-migration-blocked-'))
  const statePath = join(root, 'state.json')
  await writeFile(join(root, 'legacy-state.json'), '{"identities":[{"id":"a","session_id":"session-a","role":"Master"}],"tasks":[{"id":"task","state":"Available","owner":null}]}')
  const result = spawnSync(binary, ['--state', statePath], { input: '{"op":"migration_plan"}\n', encoding: 'utf8' })
  assert.equal(result.status, 0, result.stderr)
  const response = JSON.parse(result.stdout)
  assert.equal(response.error, 'MigrationBlocked')
  assert.deepEqual(response.issues, ['task task has no owner'])
  await assert.rejects(() => readFile(join(root, 'journal.jsonl')))
})

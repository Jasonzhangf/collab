import test from 'node:test'
import assert from 'node:assert/strict'
import { mkdir, mkdtemp, readFile, realpath } from 'node:fs/promises'
import { spawn } from 'node:child_process'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'

const packageRoot = fileURLToPath(new URL('..', import.meta.url))

function run(cwd, args = []) {
  return new Promise((resolve) => {
    const child = spawn(process.execPath, [join(packageRoot, 'bin/collab-v2-init.mjs'), ...args], { cwd, stdio: ['ignore', 'pipe', 'pipe'] })
    let stdout = ''
    let stderr = ''
    child.stdout.on('data', (chunk) => { stdout += chunk })
    child.stderr.on('data', (chunk) => { stderr += chunk })
    child.on('close', (code) => resolve({ code, stdout, stderr }))
  })
}

test('init creates environment-owned role-free control skeleton', async () => {
  const cwd = await mkdtemp(join(tmpdir(), 'collab-v2-init-'))
  const execution = await run(cwd)
  assert.equal(execution.code, 0, execution.stderr)
  const result = JSON.parse(execution.stdout)
  assert.equal(result.project_root, await realpath(cwd))
  assert.equal(result.registration, 'required')
  const guidance = await readFile(join(cwd, '.agent-collab-v2', 'INIT-AGENT.md'), 'utf8')
  assert.match(guidance, /project root, session identity, and pane endpoint from that environment/)
  assert.equal(/master|worker role|auto-register/i.test(guidance), false)
})

test('deprecated upgrade cannot create a side-by-side control plane', async () => {
  const cwd = await mkdtemp(join(tmpdir(), 'collab-v2-upgrade-'))
  await mkdir(join(cwd, '.agent-collab'))
  const execution = await run(cwd, ['--upgrade'])
  assert.notEqual(execution.code, 0)
  assert.match(execution.stderr, /formal collab migrate transaction/)
  await assert.rejects(() => readFile(join(cwd, '.agent-collab-v2', 'status.json')))
})

test('init rejects caller-selected project root and identity', async () => {
  const cwd = await mkdtemp(join(tmpdir(), 'collab-v2-init-input-'))
  const pathAttempt = await run(cwd, ['--cwd', '/tmp/other'])
  const identityAttempt = await run(cwd, ['--worker-json', '{}'])
  assert.notEqual(pathAttempt.code, 0)
  assert.notEqual(identityAttempt.code, 0)
})

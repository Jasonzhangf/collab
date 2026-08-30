import test from 'node:test'
import assert from 'node:assert/strict'
import { spawn } from 'node:child_process'
import { mkdtemp, readFile, realpath, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'

const packageRoot = fileURLToPath(new URL('..', import.meta.url))

test('profile config selects and persists the active profile', async () => {
  const { loadCollabConfig, selectProfile } = await import('../src/profile-config.mjs')
  const cwd = await mkdtemp(join(tmpdir(), 'collab-v2-profile-'))
  const path = join(cwd, 'collab.json')
  await writeFile(path, JSON.stringify({ active_profile: 'safe', profiles: { safe: { launcher: 'codexp' }, fast: { launcher: 'codexp-fast' } } }))
  assert.equal(selectProfile('fast', path), 'fast')
  const config = loadCollabConfig(path)
  assert.equal(config.active_profile, 'fast')
  assert.equal(config.profiles.fast.launcher, 'codexp-fast')
  assert.equal((await readFile(path, 'utf8')).includes('"path"'), false)
})

test('standalone CLI initializes and reports registered worker', async () => {
  const cwd = await mkdtemp(join(tmpdir(), 'collab-v2-stdio-'))
  const child = spawn(process.execPath, [join(packageRoot, 'bin/collab-v2.mjs')], { cwd, env: { ...process.env, COLLAB_V2_CORE_BINARY: join(packageRoot, 'target/debug/core-daemon') }, stdio: ['pipe', 'pipe', 'pipe'] })
  const lines = []
  child.stdout.setEncoding('utf8')
  child.stdout.on('data', (chunk) => lines.push(...chunk.trim().split('\n').filter(Boolean)))
  child.stdin.write(`${JSON.stringify({ op: 'register', identity: { id: 'cli-a', sessionId: 'cli-session', pane: '%1' } })}\n`)
  child.stdin.write(`${JSON.stringify({ op: 'snapshot' })}\n`)
  child.stdin.write('{"op":"stop"}\n')
  const exitCode = await new Promise((resolve) => child.on('close', resolve))
  assert.equal(exitCode, 0)
  assert.equal(JSON.parse(lines[0]).ok, true)
  assert.equal(JSON.parse(lines[1]).identities[0].id, 'cli-a')
  assert.equal('role' in JSON.parse(lines[1]).identities[0], false)
})

test('non-tmux who uses exact process cwd and reports registration required', async () => {
  const cwd = await mkdtemp(join(tmpdir(), 'collab-v2-unregistered-'))
  const env = { ...process.env, COLLAB_V2_CORE_BINARY: join(packageRoot, 'target/debug/core-daemon') }
  delete env.TMUX_PANE
  const child = spawn(process.execPath, [join(packageRoot, 'bin/collab.mjs'), 'who'], { cwd, env, stdio: ['ignore', 'pipe', 'pipe'] })
  let stdout = ''
  let stderr = ''
  child.stdout.on('data', (chunk) => { stdout += chunk })
  child.stderr.on('data', (chunk) => { stderr += chunk })
  const exitCode = await new Promise((resolve) => child.on('close', resolve))
  assert.equal(exitCode, 0, stderr)
  const result = JSON.parse(stdout)
  assert.equal(result.registration, 'required')
  assert.equal(result.identity, null)
  assert.equal(result.project_root, await realpath(cwd))
})

test('MCP exposes only role-free environment-owned tools', async () => {
  const cwd = await mkdtemp(join(tmpdir(), 'collab-v2-mcp-'))
  const env = { ...process.env, COLLAB_V2_CORE_BINARY: join(packageRoot, 'target/debug/core-daemon') }
  delete env.TMUX_PANE
  const child = spawn(process.execPath, [join(packageRoot, 'bin/collab-mcp.mjs')], { cwd, env, stdio: ['pipe', 'pipe', 'pipe'] })
  child.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', id: 1, method: 'tools/list' })}\n`)
  const response = await new Promise((resolveResponse, reject) => {
    let buffer = ''
    child.stdout.on('data', (chunk) => {
      buffer += chunk
      if (buffer.includes('\n')) resolveResponse(JSON.parse(buffer.slice(0, buffer.indexOf('\n'))))
    })
    child.on('error', reject)
  })
  const names = response.result.tools.map(({ name }) => name)
  assert.deepEqual(names, ['collab_register', 'collab_context', 'collab_task_register', 'collab_task_update', 'collab_task_wait', 'collab_send_resource_notice', 'collab_notify_methods', 'collab_notify_subscribe', 'collab_notify_status', 'collab_notify_unsubscribe', 'collab_migrate_inspect', 'collab_migrate_plan', 'collab_migrate_apply', 'collab_migrate_verify', 'collab_migrate_resume'])
  assert.equal(names.some((name) => /master|claim|presence/.test(name)), false)
  assert.equal(JSON.stringify(response.result.tools).includes('projectRoot'), false)
  child.kill('SIGTERM')
  await new Promise((resolveExit) => child.on('close', resolveExit))
})

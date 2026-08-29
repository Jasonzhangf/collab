import test from 'node:test'
import assert from 'node:assert/strict'
import { spawn } from 'node:child_process'
import { mkdtemp, readFile, writeFile } from 'node:fs/promises'
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
  const child = spawn(process.execPath, ['bin/collab-v2.mjs', '--cwd', '/tmp/project'], { stdio: ['pipe', 'pipe', 'pipe'] })
  const lines = []
  child.stdout.setEncoding('utf8')
  child.stdout.on('data', (chunk) => lines.push(...chunk.trim().split('\n').filter(Boolean)))
  child.stdin.write(`${JSON.stringify({ op: 'register', worker: { agentId: 'cli-a', kind: 'generic', cwd: '/tmp/project', panelId: 'cli-panel', capabilities: [], endpoints: [] } })}\n`)
  child.stdin.write(`${JSON.stringify({ op: 'snapshot' })}\n`)
  child.stdin.write('{"op":"stop"}\n')
  const exitCode = await new Promise((resolve) => child.on('close', resolve))
  assert.equal(exitCode, 0)
  assert.equal(JSON.parse(lines[0]).role, 'master')
  assert.equal(JSON.parse(lines[1]).workers[0].agentId, 'cli-a')
})

test('whoami and test report required registration without crashing', async () => {
  const cwd = await mkdtemp(join(tmpdir(), 'collab-v2-unregistered-'))
  const child = spawn(process.execPath, [join(packageRoot, 'bin/collab.mjs'), 'whoami'], { cwd, env: { ...process.env, TMUX_PANE: '%unregistered' }, stdio: ['ignore', 'pipe', 'pipe'] })
  let stdout = ''
  let stderr = ''
  child.stdout.on('data', (chunk) => { stdout += chunk })
  child.stderr.on('data', (chunk) => { stderr += chunk })
  const exitCode = await new Promise((resolve) => child.on('close', resolve))
  assert.equal(exitCode, 0, stderr)
  const result = JSON.parse(stdout)
  assert.equal(result.registration, 'required')
  assert.equal(result.identity, null)
  assert.equal(result.panelId, '%unregistered')
})

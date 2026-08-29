import test from 'node:test'
import assert from 'node:assert/strict'
import { mkdtemp, readFile } from 'node:fs/promises'
import { realpathSync } from 'node:fs'
import { spawn } from 'node:child_process'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'

const packageRoot = fileURLToPath(new URL('..', import.meta.url))

function run(cwd, args = []) {
  return new Promise((resolve, reject) => {
    const child = spawn(process.execPath, ['bin/collab-v2-init.mjs', '--cwd', cwd, ...args], { cwd: packageRoot, stdio: ['ignore', 'pipe', 'pipe'] })
    let stdout = ''
    let stderr = ''
    child.stdout.on('data', (chunk) => { stdout += chunk })
    child.stderr.on('data', (chunk) => { stderr += chunk })
    child.on('close', (code) => code === 0 ? resolve(JSON.parse(stdout)) : reject(new Error(`${code}: ${stderr}`)))
  })
}

test('init creates new-workspace control plane and agent prompts', async () => {
  const cwd = await mkdtemp(join(tmpdir(), 'collab-v2-init-'))
  const result = await run(cwd)
  assert.equal(result.registration, 'required')
  assert.equal(result.legacyPresent, false)
  assert.match(await readFile(join(cwd, '.agent-collab-v2', 'INIT-AGENT.md'), 'utf8'), /Registration contract/)
})

test('upgrade creates an isolated v2 control plane beside legacy files and auto-registers', async () => {
  const cwd = await mkdtemp(join(tmpdir(), 'collab-v2-upgrade-'))
  const { mkdir } = await import('node:fs/promises')
  await mkdir(join(cwd, '.agent-collab'))
  const worker = JSON.stringify({ agentId: 'init-worker', kind: 'generic', cwd, panelId: 'init-panel', capabilities: [], endpoints: [] })
  const result = await run(cwd, ['--upgrade', '--worker-json', worker])
  assert.equal(result.mode, 'upgrade-coexistence')
  assert.equal(result.registration.role, 'master')
  assert.equal(result.registration.cwd, realpathSync(cwd))
})

test('sequential registrations in one workspace preserve the first master', async () => {
  const cwd = await mkdtemp(join(tmpdir(), 'collab-v2-master-lock-'))
  const first = JSON.stringify({ agentId: 'pane-one', kind: 'codex', cwd, panelId: 'pane-one', capabilities: [], endpoints: [] })
  const second = JSON.stringify({ agentId: 'pane-two', kind: 'codex', cwd, panelId: 'pane-two', capabilities: [], endpoints: [] })
  const third = JSON.stringify({ agentId: 'pane-three', kind: 'codex', cwd, panelId: 'pane-three', capabilities: [], endpoints: [] })
  assert.equal((await run(cwd, ['--worker-json', first])).registration.role, 'master')
  assert.equal((await run(cwd, ['--worker-json', second])).registration.role, 'worker')
  assert.equal((await run(cwd, ['--worker-json', third])).registration.role, 'worker')
})

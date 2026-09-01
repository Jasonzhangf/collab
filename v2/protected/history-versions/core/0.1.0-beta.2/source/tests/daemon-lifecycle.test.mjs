import test from 'node:test'
import assert from 'node:assert/strict'
import { mkdtemp, readFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { spawn } from 'node:child_process'

const packageRoot = resolve('.')
const cli = join(packageRoot, 'bin/collab.mjs')
const core = join(packageRoot, 'generated/modules/core/lib/core-daemon')

function run(cwd, args) {
  return new Promise((resolveResult) => {
    const child = spawn(process.execPath, [cli, ...args], { cwd, env: { ...process.env, COLLAB_V2_CORE_BINARY: core }, stdio: ['ignore', 'pipe', 'pipe'] })
    let stdout = ''
    let stderr = ''
    child.stdout.on('data', (chunk) => { stdout += chunk })
    child.stderr.on('data', (chunk) => { stderr += chunk })
    child.on('close', (code) => resolveResult({ code, stdout, stderr }))
  })
}

test('daemon up/status/down is explicit and DOWN prevents client autostart', async () => {
  const cwd = await mkdtemp(join(tmpdir(), 'collab-v2-daemon-'))
  try {
    const up = await run(cwd, ['up'])
    assert.equal(up.code, 0, up.stderr)
    assert.equal(JSON.parse(up.stdout).running, true)
    const status = await run(cwd, ['status'])
    assert.equal(status.code, 0, status.stderr)
    const statusValue = JSON.parse(status.stdout)
    assert.equal(statusValue.running, true)
    assert.equal(typeof statusValue.pid, 'number')
    const down = await run(cwd, ['down'])
    assert.equal(down.code, 0, down.stderr)
    const marker = await readFile(join(cwd, '.agent-collab-v2', 'DOWN'), 'utf8')
    assert.match(marker, /explicitly stopped/)
    const who = await run(cwd, ['context'])
    assert.notEqual(who.code, 0)
    assert.match(who.stderr, /explicitly down/)
    const finalStatus = await run(cwd, ['status'])
    assert.equal(JSON.parse(finalStatus.stdout).running, false)
  } finally {
    await run(cwd, ['down'])
  }
})

test('second up cannot replace the exact first daemon owner', async () => {
  const cwd = await mkdtemp(join(tmpdir(), 'collab-v2-daemon-duplicate-'))
  try {
    const first = await run(cwd, ['up'])
    assert.equal(first.code, 0, first.stderr)
    const second = await run(cwd, ['up'])
    assert.equal(second.code, 0, second.stderr)
    const firstPid = JSON.parse(first.stdout).pid
    const secondPid = JSON.parse(second.stdout).pid
    assert.equal(secondPid, firstPid)
  } finally {
    await run(cwd, ['down'])
  }
})

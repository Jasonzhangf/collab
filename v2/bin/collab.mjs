#!/usr/bin/env node
import { resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { spawnSync } from 'node:child_process'
import { readFileSync, realpathSync } from 'node:fs'
import { createCollabV2 } from '../src/index.mjs'
import { createFilePersistence } from '../src/persistence.mjs'
import { loadCollabConfig } from '../src/profile-config.mjs'

const argv = process.argv.slice(2)
const reserved = new Set(['init', 'whoami', 'test', 'profile', 'app-server', 'start', 'run', 'use', 'current'])
const op = argv[0] && !argv[0].startsWith('-') && reserved.has(argv[0]) ? argv[0] : 'start'
const arg = (name, fallback = null) => {
  const index = argv.indexOf(name)
  return index >= 0 ? argv[index + 1] : fallback
}
if (argv.includes('--cwd')) throw new Error('collab is workspace-local; run it from the target directory without --cwd')
const cwd = realpathSync(resolve(process.cwd()))
const statePath = resolve(cwd, '.agent-collab-v2/state.json')
const initScript = fileURLToPath(new URL('./collab-v2-init.mjs', import.meta.url))
const config = loadCollabConfig(arg('--config') ?? undefined)

if (argv.includes('--help') || argv.includes('-h')) {
  process.stdout.write('Usage: collab [--upgrade] | collab init|whoami|test|profile|app-server ... (current directory only)\n')
  process.exit(0)
}

function registrationArgs() {
  const workerJson = arg('--worker-json')
  if (workerJson) {
    const worker = JSON.parse(workerJson.startsWith('{') ? workerJson : readFileSync(resolve(cwd, workerJson), 'utf8'))
    return ['--worker-json', JSON.stringify(worker)]
  }
  const agentId = arg('--agent-id')
  if (!agentId) return []
  const capabilities = (arg('--capabilities', '') ?? '').split(',').map((item) => item.trim()).filter(Boolean)
  const endpoints = arg('--endpoint-json') ? JSON.parse(arg('--endpoint-json')) : []
  const tmuxTarget = arg('--tmux-target', process.env.TMUX_PANE)
  if (capabilities.includes('tmux') && endpoints.length === 0 && tmuxTarget) endpoints.push({ type: 'tmux', target: tmuxTarget })
  const worker = { agentId, kind: arg('--kind', 'generic'), cwd, panelId: arg('--panel-id', process.env.TMUX_PANE ?? agentId), capabilities, endpoints }
  return ['--worker-json', JSON.stringify(worker)]
}

if (op === 'init') {
  const workerArgs = registrationArgs()
  if (workerArgs.length === 0) {
    const result = spawnSync(process.execPath, [initScript, '--cwd', cwd, ...(argv.includes('--upgrade') ? ['--upgrade'] : [])], { encoding: 'utf8' })
    if (result.status !== 0) throw new Error(result.stderr || `collab init failed: ${result.status}`)
    process.stdout.write(`${JSON.stringify({ ...JSON.parse(result.stdout), communication: 'tmux', registration: 'required' })}\n`)
  } else {
    const result = spawnSync(process.execPath, [initScript, '--cwd', cwd, ...(argv.includes('--upgrade') ? ['--upgrade'] : []), ...workerArgs], { encoding: 'utf8' })
    if (result.status !== 0) throw new Error(result.stderr || `collab init failed: ${result.status}`)
    const initialized = JSON.parse(result.stdout)
    process.stdout.write(`${JSON.stringify({ ...initialized, communication: 'tmux' })}\n`)
  }
  process.exit(0)
}

if (op === 'profile') {
  if (argv[1] === 'show') {
    const result = spawnSync(config.profiles[config.active_profile].launcher ?? 'codexp', ['current'], { cwd, encoding: 'utf8' })
    if (result.status !== 0) throw new Error(result.stderr || 'unable to read Codex profile')
    process.stdout.write(`${JSON.stringify({ codex_profile: result.stdout.trim() })}\n`)
    process.exit(0)
  }
  if (argv[1] === 'use' && argv[2]) {
    const result = spawnSync(config.profiles[config.active_profile].launcher ?? 'codexp', ['use', argv[2]], { cwd, stdio: 'inherit' })
    process.exit(result.status ?? 1)
  }
  throw new Error('Usage: collab use <oauth|profile> | collab current')
}

if (op === 'use' || op === 'current') {
  const args = op === 'use' ? ['use', argv[1]] : ['current']
  if (op === 'use' && (!argv[1] || argv.length !== 2)) throw new Error('Usage: collab use <oauth|profile>')
  if (op === 'current' && argv.length !== 1) throw new Error('Usage: collab current')
  const result = spawnSync(config.profiles[config.active_profile].launcher ?? 'codexp', args, { cwd, stdio: 'inherit' })
  process.exit(result.status ?? 1)
}

if (op === 'start' || op === 'run') {
  const init = spawnSync(process.execPath, [initScript, '--cwd', cwd, ...(argv.includes('--upgrade') ? ['--upgrade'] : [])], { encoding: 'utf8' })
  if (init.status !== 0) throw new Error(init.stderr || `collab start initialization failed: ${init.status}`)
  const profile = config.profiles[config.active_profile]
  const launcher = profile.launcher ?? 'codexp'
  const forwarded = op === 'start' && argv[0] && !reserved.has(argv[0]) ? argv : argv.filter((item, index) => !(item === '--upgrade' || item === '--config' || (index > 0 && argv[index - 1] === '--config')))
  const args = [...(profile.args ?? []), ...forwarded]
  const result = spawnSync(launcher, args, { cwd, stdio: 'inherit' })
  process.exit(result.status ?? 1)
}

const runtime = await createCollabV2({ cwd, persistence: createFilePersistence(statePath), codexAppServer: {} })
try {
  const panelId = arg('--panel-id', process.env.TMUX_PANE)
  const identity = panelId ? runtime.collab.listWorkers().find((worker) => worker.panelId === panelId) ?? null : null
  if (op === 'whoami') {
    process.stdout.write(`${JSON.stringify({ identity, registration: identity ? 'registered' : 'required', panelId: panelId ?? null, snapshot: runtime.dashboard.snapshot() })}\n`)
  } else if (op === 'test') {
    const result = { identity, registration: identity ? 'registered' : 'required', panelId: panelId ?? null, lifecycle: runtime.communication.reconcileLifecycle(), snapshot: runtime.dashboard.snapshot() }
    const toAgentId = arg('--to')
    if (toAgentId && identity) result.delivery = await runtime.communication.send({ fromAgentId: identity.agentId, toAgentId, payload: { text: arg('--text', 'collab communication self-test') } })
    process.stdout.write(`${JSON.stringify(result)}\n`)
  } else throw new Error(`unknown command: ${op}`)
} finally {
  await runtime.dispose()
}

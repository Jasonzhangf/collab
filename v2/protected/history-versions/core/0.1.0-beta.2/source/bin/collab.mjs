#!/usr/bin/env node
import { existsSync, mkdirSync, readFileSync, unlinkSync, writeFileSync } from 'node:fs'
import { spawn } from 'node:child_process'
import { createHash } from 'node:crypto'
import { resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { createCollabV2 } from '../src/index.mjs'
import { createTmuxTransport } from '../src/transports/tmux.mjs'
import { resolveProjectEnvironment } from '../src/project-environment.mjs'

const argv = process.argv.slice(2)
if (argv.some((value) => ['--cwd', '--pane', '--panel-id', '--tmux-target'].includes(value))) throw new Error('project root and pane are environment-owned')
const { projectRoot: cwd, pane: inheritedPane, sessionId } = resolveProjectEnvironment()
const controlDir = resolve(cwd, '.agent-collab-v2')
mkdirSync(controlDir, { recursive: true })
const statePath = resolve(controlDir, 'state.json')
const socketId = createHash('sha256').update(cwd).digest('hex').slice(0, 20)
const socketPath = resolve('/tmp', `collab-v2-${socketId}.sock`)
const lockPath = resolve(controlDir, 'daemon.lock')
const daemonScript = fileURLToPath(new URL('../src/daemon.mjs', import.meta.url))
const coreBinary = process.env.COLLAB_V2_CORE_BINARY ?? fileURLToPath(new URL('../generated/modules/core/lib/core-daemon', import.meta.url))
const pidPath = resolve(controlDir, 'daemon.pid')
const pidIsAlive = (pid) => {
  try { process.kill(pid, 0); return true } catch (error) { return error.code === 'EPERM' }
}
const daemonPid = () => {
  if (!existsSync(pidPath)) return null
  const pid = Number(readFileSync(pidPath, 'utf8').trim())
  return Number.isInteger(pid) && pid > 1 ? pid : null
}
const daemonState = () => ({ down: existsSync(resolve(controlDir, 'DOWN')), pid: daemonPid(), socket: socketPath, running: Boolean(daemonPid() && pidIsAlive(daemonPid()) && existsSync(socketPath)) })
const waitFor = (predicate, limit = 100) => new Promise((resolveWait) => {
  let remaining = limit
  const check = () => {
    if (predicate() || remaining-- <= 0) return resolveWait(predicate())
    setTimeout(check, 50)
  }
  check()
})
const handleDaemonCommand = async (command) => {
  if (command === 'status') {
    output(daemonState())
    return
  }
  if (command === 'up') {
    try { unlinkSync(resolve(controlDir, 'DOWN')) } catch (error) { if (error.code !== 'ENOENT') throw error }
    if (!daemonState().running) {
      if (daemonPid() && !pidIsAlive(daemonPid())) {
        try { unlinkSync(pidPath) } catch (error) { if (error.code !== 'ENOENT') throw error }
      }
      const child = spawn(process.execPath, [daemonScript, '--socket', socketPath, '--state', statePath, '--core', coreBinary, '--lock', lockPath, '--pid', pidPath], { cwd, detached: true, stdio: 'ignore', env: process.env })
      child.unref()
      if (!(await waitFor(() => daemonState().running))) throw new Error('daemon failed to start')
    }
    output({ ...daemonState(), started: true })
    return
  }
  const serverPid = daemonPid()
  writeFileSync(resolve(controlDir, 'DOWN'), 'explicitly stopped\n')
  if (serverPid && pidIsAlive(serverPid)) process.kill(serverPid, 'SIGTERM')
  if (!(await waitFor(() => !daemonState().running && !existsSync(socketPath)))) throw new Error('daemon failed to stop')
  output({ ...daemonState(), down: true, stopped: true })
}
const [earlyCommand] = argv
const output = (result) => process.stdout.write(`${JSON.stringify(result)}\n`)
if (['up', 'down', 'status'].includes(earlyCommand)) {
  await handleDaemonCommand(earlyCommand)
  process.exit(0)
}
const runtime = await createCollabV2({
  cwd,
  rustCoreBinary: coreBinary,
  rustCoreState: statePath,
  rustCoreSocket: socketPath,
  tmuxTransport: createTmuxTransport(),
})
const value = (name) => {
  const index = argv.indexOf(name)
  return index >= 0 ? argv[index + 1] : null
}
const snapshot = () => runtime.collab.snapshot()
const identity = () => sessionId ? snapshot().identities.find((entry) => entry.session_id === sessionId) ?? null : null

try {
  const [command, subcommand, target] = argv
  if (!command || command === '--help' || command === '-h') {
    output({ usage: ['collab register', 'collab who', 'collab context', 'collab task register|update|wait', 'collab send', 'collab notify methods|subscribe|status|unsubscribe', 'collab migrate inspect|plan|apply|verify|resume'] })
  } else if (command === 'register') {
    if (!inheritedPane || !sessionId) throw new Error('registration requires an inherited tmux pane')
    runtime.collab.register({ id: sessionId, sessionId, pane: inheritedPane })
    output({ identity: identity(), project_root: cwd })
  } else if (command === 'who' || command === 'whoami') {
    const current = identity()
    output({ identity: current, registration: current ? 'registered' : 'required', project_root: cwd })
  } else if (command === 'context') {
    const current = identity()
    const state = snapshot()
    output({ identity: current, registration: current ? 'registered' : 'required', tasks: current ? state.tasks.filter((task) => task.owner === current.id) : [], messages: current ? state.messages.filter((message) => message.to === current.id || message.from === current.id) : [], subscriptions: current ? state.subscriptions.filter((subscription) => subscription.owner === current.id) : [] })
  } else if (command === 'task' && subcommand === 'register') {
    const current = identity()
    if (!current) throw new Error('registration required')
    runtime.collab.registerTask({ actor: current.id, taskId: target, featureId: value('--feature'), resourceId: value('--resource') })
    output(snapshot().tasks.find((task) => task.id === target))
  } else if (command === 'task' && subcommand === 'update') {
    const current = identity()
    if (!current) throw new Error('registration required')
    runtime.collab.transitionTask({ actor: current.id, taskId: target, state: value('--status') })
    output(snapshot().tasks.find((task) => task.id === target))
  } else if (command === 'task' && subcommand === 'wait') {
    const current = identity()
    if (!current) throw new Error('registration required')
    runtime.collab.waitTask({ actor: current.id, taskId: target, blockingTaskId: value('--for'), deadlineMs: Number(value('--deadline-ms')), nowMs: Date.now() })
    output(snapshot().tasks.find((task) => task.id === target))
  } else if (command === 'send') {
    const current = identity()
    if (!current) throw new Error('registration required')
    const notice = value('--notice')
    runtime.communication.send({ messageId: value('--message-id'), from: current.id, to: subcommand, notice, subject: value('--subject') })
    output(snapshot().messages.find((message) => message.id === value('--message-id')))
  } else if (command === 'notify' && subcommand === 'methods') {
    output({ methods: [{ method: 'tmux', live: true, opt_in: true, payload: 'COLLAB_NOTIFY <message-id>' }] })
  } else if (command === 'notify' && subcommand === 'subscribe') {
    const current = identity()
    if (!current) throw new Error('registration required')
    runtime.collab.subscribe({ owner: current.id, subscriptionId: value('--id'), event: value('--event'), subject: value('--subject'), expiresAtMs: Date.now() + Number(value('--ttl-ms')), nowMs: Date.now() })
    output(snapshot().subscriptions.find((subscription) => subscription.id === value('--id')))
  } else if (command === 'notify' && subcommand === 'status') {
    const current = identity()
    output(current ? snapshot().subscriptions.filter((subscription) => subscription.owner === current.id) : [])
  } else if (command === 'notify' && subcommand === 'unsubscribe') {
    const current = identity()
    if (!current) throw new Error('registration required')
    runtime.collab.unsubscribe({ owner: current.id, subscriptionId: target })
    output(snapshot().subscriptions.find((subscription) => subscription.id === target))
  } else if (command === 'migrate' && ['inspect', 'plan', 'apply', 'verify', 'resume'].includes(subcommand)) {
    const method = `migration${subcommand[0].toUpperCase()}${subcommand.slice(1)}`
    output(runtime.collab[method]())
  } else {
    throw new Error(`unknown command: ${argv.join(' ')}`)
  }
} finally {
  await runtime.dispose()
}

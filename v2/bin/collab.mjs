#!/usr/bin/env node
import { mkdirSync } from 'node:fs'
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
const runtime = await createCollabV2({
  cwd,
  rustCoreBinary: process.env.COLLAB_V2_CORE_BINARY ?? fileURLToPath(new URL('../generated/modules/core/lib/core-daemon', import.meta.url)),
  rustCoreState: resolve(controlDir, 'state.json'),
  tmuxTransport: createTmuxTransport(),
})
const value = (name) => {
  const index = argv.indexOf(name)
  return index >= 0 ? argv[index + 1] : null
}
const output = (result) => process.stdout.write(`${JSON.stringify(result)}\n`)
const snapshot = () => runtime.collab.snapshot()
const identity = () => sessionId ? snapshot().identities.find((entry) => entry.session_id === sessionId) ?? null : null

try {
  const [command, subcommand, target] = argv
  if (!command || command === '--help' || command === '-h') {
    output({ usage: ['collab register', 'collab who', 'collab context', 'collab task register|update|wait', 'collab send', 'collab notify methods|subscribe|status'] })
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
  } else {
    throw new Error(`unknown command: ${argv.join(' ')}`)
  }
} finally {
  await runtime.dispose()
}

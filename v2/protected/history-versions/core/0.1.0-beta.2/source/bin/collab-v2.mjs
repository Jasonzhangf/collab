#!/usr/bin/env node
import readline from 'node:readline'
import { mkdirSync, realpathSync } from 'node:fs'
import { resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { createCollabV2 } from '../src/index.mjs'

if (process.argv.includes('--cwd') || process.argv.includes('--state')) throw new Error('cwd and state are environment-owned')
const cwd = realpathSync(resolve(process.cwd()))
const controlDir = resolve(cwd, '.agent-collab-v2')
mkdirSync(controlDir, { recursive: true })
const runtime = await createCollabV2({
  cwd,
  rustCoreBinary: process.env.COLLAB_V2_CORE_BINARY ?? fileURLToPath(new URL('../generated/modules/core/lib/core-daemon', import.meta.url)),
  rustCoreState: resolve(controlDir, 'state.json'),
})
const rl = readline.createInterface({ input: process.stdin, crlfDelay: Infinity })
const reply = (value) => process.stdout.write(`${JSON.stringify(value)}\n`)

for await (const line of rl) {
  if (!line.trim()) continue
  try {
    const command = JSON.parse(line)
    if (command.op === 'register') reply(runtime.collab.register(command.identity))
    else if (command.op === 'register_task') reply(runtime.collab.registerTask(command))
    else if (command.op === 'transition_task') reply(runtime.collab.transitionTask(command))
    else if (command.op === 'wait_task') reply(runtime.collab.waitTask(command))
    else if (command.op === 'subscribe') reply(runtime.collab.subscribe(command))
    else if (command.op === 'send_resource_notice') reply(runtime.communication.send(command))
    else if (command.op === 'snapshot') reply(runtime.collab.snapshot())
    else if (command.op === 'stop') break
    else throw new Error(`unknown operation: ${command.op}`)
  } catch (error) {
    reply({ ok: false, error: error.message })
  }
}
rl.close()
await runtime.dispose()

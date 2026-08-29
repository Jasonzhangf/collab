#!/usr/bin/env node
import readline from 'node:readline'
import { createCollabV2 } from '../src/index.mjs'
import { createFilePersistence } from '../src/persistence.mjs'

const cwdArg = process.argv.indexOf('--cwd')
const cwd = cwdArg >= 0 ? process.argv[cwdArg + 1] : process.cwd()
if (!cwd) throw new Error('--cwd requires a path')
const stateArg = process.argv.indexOf('--state')
const persistence = stateArg >= 0 ? createFilePersistence(process.argv[stateArg + 1]) : undefined

const runtime = await createCollabV2({ cwd, persistence, rustCoreBinary: new URL('../generated/modules/core/lib/core-daemon', import.meta.url).pathname })
const rl = readline.createInterface({ input: process.stdin, crlfDelay: Infinity })
const reply = (value) => process.stdout.write(`${JSON.stringify(value)}\n`)

for await (const line of rl) {
  if (!line.trim()) continue
  try {
    const command = JSON.parse(line)
    if (command.op === 'register') reply(await runtime.collab.register(command.worker))
    else if (command.op === 'whoami') reply(runtime.collab.whoami(command.panelId))
    else if (command.op === 'snapshot') reply(runtime.dashboard.snapshot())
    else if (command.op === 'stop') break
    else throw new Error(`unknown operation: ${command.op}`)
  } catch (error) {
    reply({ error: error.message })
  }
}
rl.close()
await runtime.dispose()

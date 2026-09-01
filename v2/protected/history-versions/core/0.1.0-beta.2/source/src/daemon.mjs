#!/usr/bin/env node
import { createServer } from 'node:net'
import { closeSync, mkdirSync, openSync, unlinkSync, writeFileSync } from 'node:fs'
import { spawnSync } from 'node:child_process'
import { dirname } from 'node:path'

const value = (name) => {
  const index = process.argv.indexOf(name)
  return index >= 0 ? process.argv[index + 1] : null
}
const socket = value('--socket')
const state = value('--state')
const core = value('--core')
const lockPath = value('--lock')
const pidPath = value('--pid')
if (!socket || !state || !core || !lockPath || !pidPath) throw new Error('daemon requires environment-owned socket, state, core, lock, and pid')
mkdirSync(dirname(socket), { recursive: true })
let lock
try {
  lock = openSync(lockPath, 'wx')
  writeFileSync(lockPath, `${process.pid}\n`)
} catch (error) {
  throw new Error(`DuplicateDaemon: ${error.message}`)
}
try { unlinkSync(socket) } catch (error) { if (error.code !== 'ENOENT') throw error }
const cleanup = () => {
  try { server.close() } catch {}
  try { unlinkSync(socket) } catch {}
  try { closeSync(lock) } catch {}
  try { unlinkSync(lockPath) } catch {}
  try { unlinkSync(pidPath) } catch {}
}
const server = createServer((connection) => {
  let buffer = ''
  connection.setEncoding('utf8')
  connection.on('data', (chunk) => {
    buffer += chunk
    while (buffer.includes('\n')) {
      const index = buffer.indexOf('\n')
      const line = buffer.slice(0, index)
      buffer = buffer.slice(index + 1)
      if (!line.trim()) continue
      const result = spawnSync(core, ['--state', state], { input: `${line}\n`, encoding: 'utf8' })
      if (result.error || result.status !== 0) {
        connection.write(`${JSON.stringify({ ok: false, error: result.error?.message ?? result.stderr.trim() ?? `core exited ${result.status}` })}\n`)
      } else {
        connection.write(`${result.stdout.trim().split('\n').at(-1)}\n`)
      }
    }
  })
})
server.listen(socket)
writeFileSync(pidPath, `${process.pid}\n`)
for (const signal of ['SIGTERM', 'SIGINT']) process.on(signal, () => { cleanup(); process.exit(0) })
process.on('exit', cleanup)

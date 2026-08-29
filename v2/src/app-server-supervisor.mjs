import { existsSync, readFileSync, writeFileSync } from 'node:fs'
import { mkdir, unlink } from 'node:fs/promises'
import { spawn } from 'node:child_process'
import { createServer } from 'node:net'
import { resolve } from 'node:path'

async function freePort() {
  const server = createServer()
  await new Promise((resolvePromise, reject) => { server.once('error', reject); server.listen(0, '127.0.0.1', resolvePromise) })
  const port = server.address().port
  await new Promise((resolvePromise, reject) => { server.close((error) => error ? reject(error) : resolvePromise()) })
  return port
}

function substitute(value, values) {
  return value.replaceAll('{address}', values.address).replaceAll('{port}', String(values.port)).replaceAll('{cwd}', values.cwd)
}

export async function createAppServerSupervisor({ cwd, config = {} }) {
  const controlDir = resolve(cwd, '.agent-collab-v2')
  const recordPath = resolve(controlDir, 'app-server.json')
  const app = config.app_server ?? {}
  const command = app.command ?? 'codex'
  const argsTemplate = app.args ?? ['app-server', '--listen', '{address}']
  let record = existsSync(recordPath) ? JSON.parse(readFileSync(recordPath, 'utf8')) : null
  async function ping(address) {
    const socket = new WebSocket(address)
    let id = 0
    return new Promise((resolvePromise, reject) => {
      const timer = setTimeout(() => { socket.close(); reject(new Error('app-server ping timeout')) }, app.ping_timeout_ms ?? 5000)
      socket.addEventListener('open', () => socket.send(JSON.stringify({ jsonrpc: '2.0', id: ++id, method: 'initialize', params: { clientInfo: { name: 'collab-v2-supervisor', version: '0.1.0-beta.2' } } })))
      socket.addEventListener('message', (event) => {
        const message = JSON.parse(event.data)
        if (message.id !== id) return
        clearTimeout(timer)
        socket.send(JSON.stringify({ jsonrpc: '2.0', method: 'initialized', params: {} }))
        socket.close()
        resolvePromise({ ok: true, address })
      })
      socket.addEventListener('error', () => { clearTimeout(timer); reject(new Error('app-server ping failed')) }, { once: true })
    })
  }
  async function ensure() {
    await mkdir(controlDir, { recursive: true })
    if (record?.pid) {
      try { process.kill(record.pid, 0); return { ...record, status: 'started' } } catch { record = null }
    }
    const port = await freePort()
    const address = app.address ?? `ws://127.0.0.1:${port}`
    const values = { address, port, cwd }
    const args = argsTemplate.map((item) => substitute(item, values))
    const child = spawn(command, args, { cwd, detached: true, stdio: 'ignore' })
    child.unref()
    record = { pid: child.pid, command, args, address, cwd, startedAt: Date.now(), status: 'starting' }
    writeFileSync(recordPath, `${JSON.stringify(record, null, 2)}\n`)
    await new Promise((resolvePromise) => setTimeout(resolvePromise, 250))
    try { process.kill(child.pid, 0) } catch (error) { throw new Error(`app-server exited during startup: ${error.message}`) }
    record = { ...record, status: 'started' }
    writeFileSync(recordPath, `${JSON.stringify(record, null, 2)}\n`)
    return record
  }
  async function stop() {
    if (!record) return { status: 'stopped' }
    if (record.pid) {
      try { process.kill(record.pid, 'SIGTERM') } catch (error) { if (error.code !== 'ESRCH') throw error }
    }
    try { await unlink(recordPath) } catch (error) { if (error.code !== 'ENOENT') throw error }
    const previous = record
    record = null
    return { status: 'stopped', pid: previous.pid, address: previous.address }
  }
  return { ensure, ping, stop, status: () => record ? { ...record } : { status: 'stopped' } }
}

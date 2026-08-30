#!/usr/bin/env node
import { createConnection } from 'node:net'

const socket = process.argv[2]
const line = process.argv.slice(3).join(' ')
if (!socket || !line) throw new Error('daemon client requires socket and command')
const connection = createConnection(socket)
let buffer = ''
connection.setEncoding('utf8')
connection.on('data', (chunk) => {
  buffer += chunk
  const index = buffer.indexOf('\n')
  if (index < 0) return
  process.stdout.write(`${buffer.slice(0, index)}\n`)
  connection.end()
})
connection.on('error', (error) => { process.stderr.write(`${error.message}\n`); process.exitCode = 2 })
connection.on('connect', () => connection.write(`${line}\n`))

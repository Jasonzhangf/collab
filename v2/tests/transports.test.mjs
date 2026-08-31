import test from 'node:test'
import assert from 'node:assert/strict'
import { createTmuxTransport } from '../src/transports/tmux.mjs'

test('tmux sends one literal short wake command sequence', async () => {
  const calls = []
  const transport = createTmuxTransport({ run: async (...args) => calls.push(args) })
  const receipt = await transport.deliver({ target: 'session:1.0', messageId: 'message-1' })
  assert.deepEqual(receipt, { protocol: 'tmux', target: 'session:1.0', messageId: 'message-1' })
  assert.deepEqual(calls, [['tmux', ['send-keys', '-t', 'session:1.0', '--', 'COLLAB_NOTIFY message-1', 'Enter']]])
})

test('tmux rejects body injection and missing endpoint', async () => {
  const transport = createTmuxTransport({ run: async () => assert.fail('tmux must not run') })
  await assert.rejects(() => transport.deliver({ target: 'session:1.0', messageId: 'message-1', payload: { text: 'body' } }), /payload is forbidden/)
  await assert.rejects(() => transport.deliver({ messageId: 'message-1' }), /target is required/)
})

test('tmux rejects terminal control characters before process invocation', async () => {
  const calls = []
  const transport = createTmuxTransport({ run: async (...args) => calls.push(args) })
  for (const messageId of ['bad\nline', 'bad\rline', 'bad\0line', 'bad\tline', `bad${String.fromCharCode(0x1b)}line`, `bad${String.fromCharCode(0x7f)}line`, `bad${String.fromCharCode(0x85)}line`]) {
    await assert.rejects(() => transport.deliver({ target: 'session:1.0', messageId }), /terminal control/)
  }
  assert.deepEqual(calls, [])
})

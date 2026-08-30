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

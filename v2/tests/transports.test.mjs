import test from 'node:test'
import assert from 'node:assert/strict'
import { mkdtemp } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { createMailboxTransport } from '../src/transports/mailbox.mjs'
import { createTmuxTransport } from '../src/transports/tmux.mjs'

test('tmux adaptor owns only non-Codex pane delivery', async () => {
  const calls = []
  const transport = createTmuxTransport({ run: async (...args) => calls.push(args) })
  const receipt = await transport.deliver({ endpoint: { target: 'session:1.0' }, payload: { text: 'hello' } })
  assert.deepEqual(receipt, { protocol: 'tmux', target: 'session:1.0' })
  assert.deepEqual(calls[0], ['tmux', ['send-keys', '-t', 'session:1.0', '--', 'hello', 'Enter']])
})

test('mailbox adaptor supports durable receive and acknowledgement', async () => {
  const root = await mkdtemp(join(tmpdir(), 'collab-v2-mailbox-'))
  const transport = createMailboxTransport({ root })
  const endpoint = { mailboxId: 'worker-b' }
  await transport.deliver({ endpoint, messageId: 'm1', fromAgentId: 'a', toAgentId: 'b', payload: { text: 'hello' } })
  assert.equal((await transport.receive({ endpoint })).length, 1)
  await transport.acknowledge({ endpoint, messageId: 'm1', agentId: 'b' })
  assert.deepEqual(await transport.receive({ endpoint }), [])
})

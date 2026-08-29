import { appendFile, readFile } from 'node:fs/promises'

export function createMailboxTransport({ root }) {
  if (typeof root !== 'string' || root.length === 0) throw new TypeError('mailbox root is required')
  return {
    async deliver({ endpoint, messageId, fromAgentId, toAgentId, payload }) {
      if (!endpoint?.mailboxId) throw new Error('mailbox endpoint requires mailboxId')
      if (payload === undefined) throw new TypeError('mailbox payload is required')
      const path = `${root}/${endpoint.mailboxId}.jsonl`
      const record = { messageId, fromAgentId, toAgentId, payload, queuedAt: Date.now() }
      await appendFile(path, `${JSON.stringify(record)}\n`, 'utf8')
      return { protocol: 'mailbox', mailboxId: endpoint.mailboxId, path }
    },
    async receive({ endpoint }) {
      const path = `${root}/${endpoint.mailboxId}.jsonl`
      let content
      try { content = await readFile(path, 'utf8') } catch (error) {
        if (error.code === 'ENOENT') return []
        throw error
      }
      const records = content.split('\n').filter(Boolean).map((line) => JSON.parse(line))
      const acknowledged = new Set(records.filter((record) => record.kind === 'ack').map((record) => record.messageId))
      return records.filter((record) => record.messageId && record.kind !== 'ack' && !acknowledged.has(record.messageId))
    },
    async acknowledge({ messageId, endpoint }) {
      const path = `${root}/${endpoint.mailboxId}.jsonl`
      await appendFile(path, `${JSON.stringify({ kind: 'ack', messageId, ackedAt: Date.now() })}\n`, 'utf8')
      return { protocol: 'mailbox', mailboxId: endpoint.mailboxId, messageId, acknowledged: true }
    },
  }
}

import { execFile } from 'node:child_process'
import { promisify } from 'node:util'

const execFileAsync = promisify(execFile)

export function createTmuxTransport({ run = execFileAsync } = {}) {
  return {
    async deliver({ endpoint, payload }) {
      if (!endpoint?.target) throw new Error('tmux endpoint requires target')
      if (!payload || typeof payload.text !== 'string') throw new TypeError('tmux payload requires text')
      await run('tmux', ['send-keys', '-t', endpoint.target, '--', payload.text, 'Enter'])
      return { protocol: 'tmux', target: endpoint.target }
    },
  }
}

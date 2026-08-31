import { execFile } from 'node:child_process'
import { promisify } from 'node:util'

const execFileAsync = promisify(execFile)
const TERMINAL_CONTROL = /[\u0000-\u001f\u007f-\u009f]/

export function createTmuxTransport({ run = execFileAsync } = {}) {
  return Object.freeze({
    async deliver(input) {
      if (input?.payload !== undefined) throw new TypeError('tmux payload is forbidden')
      if (typeof input?.target !== 'string' || input.target.length === 0) throw new TypeError('tmux target is required')
      if (typeof input.messageId !== 'string' || input.messageId.length === 0) throw new TypeError('tmux messageId is required')
      if (TERMINAL_CONTROL.test(input.messageId)) throw new TypeError('tmux messageId contains terminal control characters')
      const wake = `COLLAB_NOTIFY ${input.messageId}`
      await run('tmux', ['send-keys', '-t', input.target, '--', wake, 'Enter'])
      return Object.freeze({ protocol: 'tmux', target: input.target, messageId: input.messageId })
    },
  })
}

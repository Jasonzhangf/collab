import { execFileSync } from 'node:child_process'
import { realpathSync } from 'node:fs'
import { resolve } from 'node:path'

export function resolveProjectEnvironment(env = process.env, processCwd = process.cwd()) {
  const pane = env.TMUX_PANE ?? null
  if (!pane) return Object.freeze({ projectRoot: realpathSync(resolve(processCwd)), pane: null, sessionId: null })
  const facts = execFileSync('tmux', ['display-message', '-p', '-t', pane, '#{pane_current_path}\t#{session_name}'], { encoding: 'utf8', env }).trim().split('\t')
  if (facts.length !== 2 || !facts[0] || !facts[1]) throw new Error('unable to resolve inherited tmux pane facts')
  return Object.freeze({ projectRoot: realpathSync(resolve(facts[0])), pane, sessionId: facts[1] })
}

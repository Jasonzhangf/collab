import { spawnSync } from 'node:child_process'
import { mkdirSync, mkdtempSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { fileURLToPath } from 'node:url'
import { dirname, join } from 'node:path'

export function createRustCoreClient(config = {}) {
  const binary = config.rustCoreBinary ?? fileURLToPath(new URL('../generated/modules/core/lib/core-daemon', import.meta.url))
  const state = config.rustCoreState ?? join(mkdtempSync(join(tmpdir(), 'collab-v2-rust-core-')), 'state.json')
  mkdirSync(dirname(state), { recursive: true })
  const invoke = (command) => {
    const result = spawnSync(binary, ['--state', state], { input: `${JSON.stringify(command)}\n`, encoding: 'utf8' })
    if (result.error) throw result.error
    if (result.status !== 0) throw new Error(`rust core exited with status ${result.status}: ${result.stderr ?? ''}`.trim())
    if (!result.stdout.trim()) throw new Error(`rust core returned no response for ${command.op}`)
    const response = JSON.parse(result.stdout.trim().split('\n').at(-1))
    if (!response.ok) throw new Error(`rust core rejected ${command.op}: ${response.error}`)
    return response
  }
  return {
    register: (identity) => invoke({ op: 'register', identity }),
    createTask: (actor, taskId) => invoke({ op: 'create_task', actor, task_id: taskId }),
    claim: (actor, taskId) => invoke({ op: 'claim', actor, task_id: taskId }),
    transition: (actor, taskId, stateName) => invoke({ op: 'transition', actor, task_id: taskId, state: stateName }),
    snapshot: () => invoke({ op: 'snapshot' }),
  }
}

import { spawnSync } from 'node:child_process'
import { resolve } from 'node:path'

export function createRustCoreClient(config = {}) {
  const binary = config.rustCoreBinary ?? resolve(config.cwd ?? process.cwd(), 'generated/modules/core/lib/core-daemon')
  const state = config.rustCoreState ?? resolve(config.cwd ?? process.cwd(), '.collab-v2-core-state.json')
  const invoke = (command) => {
    const result = spawnSync(binary, ['--state', state], { input: `${JSON.stringify(command)}\n`, encoding: 'utf8' })
    if (result.error) throw result.error
    const response = JSON.parse(result.stdout.trim().split('\n').at(-1))
    if (!response.ok) throw new Error(`rust core rejected ${command.op}: ${response.error}`)
    return response
  }
  return {
    register: (identity) => invoke({ op: 'register', identity }),
    createTask: (actor, taskId) => invoke({ op: 'create_task', actor, task_id: taskId }),
    claim: (actor, taskId) => invoke({ op: 'claim', actor, task_id: taskId }),
    transition: (actor, taskId, stateName) => invoke({ op: 'transition', actor, task_id: taskId, state: stateName }),
  }
}

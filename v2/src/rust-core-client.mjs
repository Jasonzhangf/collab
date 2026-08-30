import { spawnSync } from 'node:child_process'
import { mkdirSync } from 'node:fs'
import { dirname } from 'node:path'
import { resolve } from 'node:path'

export function createRustCoreClient(config = {}) {
  const binary = config.rustCoreBinary ?? resolve(config.cwd ?? process.cwd(), 'generated/modules/core/lib/core-daemon')
  const state = config.rustCoreState ?? resolve(config.cwd ?? process.cwd(), '.collab-v2-core-state.json')
  mkdirSync(dirname(state), { recursive: true })
  const invoke = (command) => {
    const result = spawnSync(binary, ['--state', state], { input: `${JSON.stringify(command)}\n`, encoding: 'utf8' })
    if (result.error) throw result.error
    const response = JSON.parse(result.stdout.trim().split('\n').at(-1))
    if (!response.ok) throw new Error(`rust core rejected ${command.op}: ${response.error}`)
    return response
  }
  return {
    register: (identity) => invoke({ op: 'register', identity }),
    registerTask: ({ actor, taskId, featureId, resourceId }) => invoke({ op: 'register_task', actor, task_id: taskId, feature_id: featureId, resource_id: resourceId }),
    transitionTask: ({ actor, taskId, state }) => invoke({ op: 'transition_task', actor, task_id: taskId, state }),
    waitTask: ({ actor, taskId, blockingTaskId, deadlineMs, nowMs }) => invoke({ op: 'wait_task', actor, task_id: taskId, blocking_task_id: blockingTaskId, deadline_ms: deadlineMs, now_ms: nowMs }),
    subscribe: ({ owner, subscriptionId, event, subject, expiresAtMs, nowMs }) => invoke({ op: 'subscribe', owner, subscription_id: subscriptionId, event, subject, expires_at_ms: expiresAtMs, now_ms: nowMs }),
    sendResourceNotice: ({ messageId, from, to, notice, subject }) => invoke({ op: 'send_resource_notice', message_id: messageId, from, to, notice, subject }),
    wakeAttempt: ({ messageId, agentState, succeeded, nowMs }) => invoke({ op: 'wake_attempt', message_id: messageId, agent_state: agentState, succeeded, now_ms: nowMs }),
    snapshot: () => invoke({ op: 'snapshot' }).state,
  }
}

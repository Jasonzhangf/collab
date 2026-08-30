import { spawnSync } from 'node:child_process'
import { randomUUID } from 'node:crypto'
import { mkdirSync } from 'node:fs'
import { dirname } from 'node:path'
import { resolve } from 'node:path'

export function createRustCoreClient(config = {}) {
  const binary = config.rustCoreBinary ?? resolve(config.cwd ?? process.cwd(), 'generated/modules/core/lib/core-daemon')
  const state = config.rustCoreState ?? resolve(config.cwd ?? process.cwd(), '.collab-v2-core-state.json')
  mkdirSync(dirname(state), { recursive: true })
  const invoke = (command, commandId = randomUUID()) => {
    const result = spawnSync(binary, ['--state', state], { input: `${JSON.stringify({ ...command, command_id: commandId })}\n`, encoding: 'utf8' })
    if (result.error) throw result.error
    if (result.status !== 0) throw new Error(`rust core exited ${result.status}: ${result.stderr.trim()}`)
    const output = result.stdout.trim()
    if (!output) throw new Error('rust core returned no response')
    const response = JSON.parse(output.split('\n').at(-1))
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
    beginWakeAttempt: ({ messageId, agentState, nowMs }) => invoke({ op: 'begin_wake_attempt', message_id: messageId, agent_state: agentState, now_ms: nowMs }),
    completeWakeAttempt: ({ messageId, attempt, succeeded }) => invoke({ op: 'complete_wake_attempt', message_id: messageId, attempt, succeeded }),
    migrationInspect: () => invoke({ op: 'migration_inspect' }),
    migrationPlan: () => invoke({ op: 'migration_plan' }),
    migrationApply: () => invoke({ op: 'migration_apply' }),
    migrationVerify: () => invoke({ op: 'migration_verify' }),
    migrationResume: () => invoke({ op: 'migration_resume' }),
    snapshot: () => {
      const result = spawnSync(binary, ['--state', state], { input: '{"op":"snapshot"}\n', encoding: 'utf8' })
      if (result.error) throw result.error
      if (result.status !== 0) throw new Error(`rust core exited ${result.status}: ${result.stderr.trim()}`)
      const output = result.stdout.trim()
      if (!output) throw new Error('rust core returned no response')
      const response = JSON.parse(output.split('\n').at(-1))
      if (!response.ok) throw new Error(`rust core rejected snapshot: ${response.error}`)
      return response.state
    },
  }
}

import { createRustCoreClient } from './rust-core-client.mjs'

function required(value, name) {
  if (typeof value !== 'string' || value.length === 0) throw new TypeError(`${name} is required`)
  return value
}

export const CollabCore = (ctx, config = {}) => {
  const core = config.coreClient ?? createRustCoreClient(config)
  const api = Object.freeze({
    scope: config.cwd ?? process.cwd(),
    register(input) {
      if (!input || typeof input !== 'object') throw new TypeError('identity is required')
      return core.register({
        id: required(input.id, 'id'),
        session_id: required(input.sessionId, 'sessionId'),
        pane: required(input.pane, 'pane'),
      })
    },
    registerTask(input) {
      if (!input || typeof input !== 'object') throw new TypeError('task is required')
      return core.registerTask({
        actor: required(input.actor, 'actor'),
        taskId: required(input.taskId, 'taskId'),
        featureId: required(input.featureId, 'featureId'),
        resourceId: required(input.resourceId, 'resourceId'),
      })
    },
    transitionTask(input) {
      if (!input || typeof input !== 'object') throw new TypeError('task transition is required')
      return core.transitionTask({ actor: required(input.actor, 'actor'), taskId: required(input.taskId, 'taskId'), state: required(input.state, 'state') })
    },
    waitTask(input) {
      if (!input || typeof input !== 'object') throw new TypeError('wait is required')
      return core.waitTask({
        actor: required(input.actor, 'actor'),
        taskId: required(input.taskId, 'taskId'),
        blockingTaskId: required(input.blockingTaskId, 'blockingTaskId'),
        deadlineMs: input.deadlineMs,
        nowMs: input.nowMs,
      })
    },
    subscribe(input) {
      if (!input || typeof input !== 'object') throw new TypeError('subscription is required')
      return core.subscribe({
        owner: required(input.owner, 'owner'),
        subscriptionId: required(input.subscriptionId, 'subscriptionId'),
        event: required(input.event, 'event'),
        subject: input.subject ?? null,
        expiresAtMs: input.expiresAtMs,
        nowMs: input.nowMs,
      })
    },
    sendResourceNotice(input) {
      if (!input || typeof input !== 'object') throw new TypeError('resource notice is required')
      return core.sendResourceNotice({
        messageId: required(input.messageId, 'messageId'),
        from: required(input.from, 'from'),
        to: required(input.to, 'to'),
        notice: required(input.notice, 'notice'),
        subject: required(input.subject, 'subject'),
      })
    },
    wakeAttempt(input) {
      if (!input || typeof input !== 'object') throw new TypeError('wake attempt is required')
      return core.wakeAttempt({ messageId: required(input.messageId, 'messageId'), agentState: required(input.agentState, 'agentState'), succeeded: input.succeeded === true, nowMs: input.nowMs })
    },
    snapshot: () => core.snapshot(),
  })

  ctx.provide('collab', api)
  return () => {}
}

CollabCore.provide = 'collab'

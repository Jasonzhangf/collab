import { assertWorkerRegistration, capabilityKey } from './contracts.mjs'
import { createRustCoreClient } from './rust-core-client.mjs'

export const CollabCore = (ctx, config = {}) => {
  const scope = config.cwd ?? process.cwd()
  const workers = new Map()
  const identities = new Map()
  const capabilities = new Map()
  const tasks = new Map()
  const messages = new Map()
  const persistence = config.persistence ?? null
  let projectState = 'initialized'
  const verifier = config.verifyCapability ?? (async () => true)
  const core = config.coreClient ?? (config.rustCoreBinary ? createRustCoreClient(config) : null)

  function restore(state) {
    workers.clear(); identities.clear(); capabilities.clear(); tasks.clear(); messages.clear()
    projectState = state.projectState ?? 'initialized'
    for (const worker of state.workers ?? []) { workers.set(worker.agentId, Object.freeze(worker)); identities.set(worker.panelId, worker.agentId) }
    for (const entry of state.identities ?? []) identities.set(entry[0], entry[1])
    for (const capability of state.capabilities ?? []) capabilities.set(`${capability.agentId}:${capability.id}`, capability)
    for (const task of state.tasks ?? []) tasks.set(task.taskId, Object.freeze(task))
    for (const message of state.messages ?? []) messages.set(message.messageId, Object.freeze(message))
  }
  restore(persistence?.load?.() ?? {})

  function persist() {
    persistence?.save({ projectState, workers: [...workers.values()], identities: [...identities.entries()], capabilities: [...capabilities.values()], tasks: [...tasks.values()], messages: [...messages.values()] })
  }

  function requireWorker(agentId) {
    const worker = workers.get(agentId)
    if (!worker) throw new Error(`unknown worker: ${agentId}`)
    return worker
  }

  function requirePermission(agentId, permission) {
    const worker = requireWorker(agentId)
    if (!worker.permissions.includes(permission)) throw new Error(`permission denied: ${permission}`)
    return worker
  }

  const api = {
    scope,
    async register(input) {
      assertWorkerRegistration(input)
      if (input.cwd !== scope) throw new Error(`cwd mismatch: expected ${scope}, received ${input.cwd}`)
      core?.register({ id: input.agentId, session_id: input.panelId, role: workers.size === 0 ? 'Master' : 'Worker' })
      const release = persistence?.acquireLock ? await persistence.acquireLock() : async () => {}
      try {
        if (persistence?.load) restore(persistence.load())
        if (workers.has(input.agentId)) throw new Error(`worker already registered: ${input.agentId}`)
        if (identities.has(input.panelId)) throw new Error(`panel already registered: ${input.panelId}`)
        const verified = []
        for (const raw of input.capabilities) {
          const id = capabilityKey(raw)
          if (!await verifier({ ...input, capability: id })) throw new Error(`capability verification failed: ${id}`)
          verified.push(id)
          capabilities.set(`${input.agentId}:${id}`, { agentId: input.agentId, id, verifiedAt: Date.now() })
        }
        const worker = Object.freeze({
          agentId: input.agentId,
          kind: input.kind,
          cwd: input.cwd,
          panelId: input.panelId,
          endpoints: Object.freeze([...input.endpoints]),
          capabilities: Object.freeze(verified),
          role: workers.size === 0 ? 'master' : 'worker',
          permissions: Object.freeze(workers.size === 0 ? ['register', 'assign', 'transfer', 'send', 'close-project'] : ['claim', 'send']),
          state: 'online',
          presence: Object.freeze({ status: 'online', lastSeenAt: Date.now() }),
          registeredAt: Date.now(),
        })
        workers.set(worker.agentId, worker)
        identities.set(worker.panelId, worker.agentId)
        if (projectState === 'initialized') projectState = 'accepting_workers'
        persist()
        return worker
      } finally {
        await release()
      }
    },
    activate(agentId) {
      const worker = requireWorker(agentId)
      const active = Object.freeze({ ...worker, state: 'online', presence: Object.freeze({ status: 'online', lastSeenAt: Date.now() }) })
      workers.set(agentId, active)
      persist()
      return active
    },
    close(agentId) {
      const worker = requireWorker(agentId)
      const closed = Object.freeze({ ...worker, state: 'closed', presence: Object.freeze({ status: 'offline', lastSeenAt: Date.now() }) })
      workers.set(agentId, closed)
      persist()
      return closed
    },
    whoami(panelId) {
      const agentId = identities.get(panelId)
      if (!agentId) throw new Error(`unknown panel: ${panelId}`)
      return workers.get(agentId)
    },
    transferIdentity(fromPanelId, toPanelId, actorAgentId) {
      requirePermission(actorAgentId, 'transfer')
      const agentId = identities.get(fromPanelId)
      if (!agentId) throw new Error(`unknown source panel: ${fromPanelId}`)
      if (identities.has(toPanelId)) throw new Error(`target panel already mapped: ${toPanelId}`)
      identities.delete(fromPanelId)
      identities.set(toPanelId, agentId)
      persist()
      return api.whoami(toPanelId)
    },
    listWorkers: () => [...workers.values()],
    heartbeat(agentId) {
      const worker = requireWorker(agentId)
      const online = Object.freeze({ ...worker, state: 'online', presence: Object.freeze({ status: 'online', lastSeenAt: Date.now() }) })
      workers.set(agentId, online)
      persist()
      return online.presence
    },
    markOffline(agentId) {
      const worker = requireWorker(agentId)
      const offline = Object.freeze({ ...worker, presence: Object.freeze({ status: 'offline', lastSeenAt: Date.now() }) })
      workers.set(agentId, offline)
      persist()
      return offline.presence
    },
    capability: (agentId, id) => capabilities.get(`${agentId}:${id}`) ?? null,
    createTask(input) {
      if (!input || typeof input.taskId !== 'string' || typeof input.title !== 'string') throw new TypeError('taskId and title are required')
      requirePermission(input.actorAgentId, 'assign')
      core?.createTask(input.actorAgentId, input.taskId)
      if (tasks.has(input.taskId)) throw new Error(`task already exists: ${input.taskId}`)
      const task = Object.freeze({ taskId: input.taskId, title: input.title, state: 'available', assignee: null, createdAt: Date.now() })
      tasks.set(task.taskId, task)
      persist()
      return task
    },
    claimTask(taskId, agentId) {
      const task = tasks.get(taskId)
      if (!task) throw new Error(`unknown task: ${taskId}`)
      requireWorker(agentId)
      core?.claim(agentId, taskId)
      if (task.state !== 'available') throw new Error(`task is not available: ${taskId}`)
      const claimed = Object.freeze({ ...task, state: 'working', assignee: agentId, claimedAt: Date.now() })
      tasks.set(taskId, claimed)
      persist()
      return claimed
    },
    transitionTask(taskId, state, actorAgentId = tasks.get(taskId)?.assignee) {
      const task = tasks.get(taskId)
      if (!task) throw new Error(`unknown task: ${taskId}`)
      if (actorAgentId !== task.assignee) requirePermission(actorAgentId, 'assign')
      core?.transition(actorAgentId, taskId, state[0].toUpperCase() + state.slice(1))
      const allowed = { available: ['working'], working: ['verifying', 'blocked', 'cancelled'], blocked: ['working', 'cancelled'], verifying: ['reviewing', 'working'], reviewing: ['delivered', 'working'], delivered: ['merged'], merged: ['closed'] }
      if (!allowed[task.state]?.includes(state)) throw new Error(`invalid task transition: ${task.state} -> ${state}`)
      const next = Object.freeze({ ...task, state, updatedAt: Date.now() })
      tasks.set(taskId, next)
      persist()
      return next
    },
    listTasks: () => [...tasks.values()],
    createMessage(input) {
      if (!input || typeof input.messageId !== 'string' || typeof input.fromAgentId !== 'string' || typeof input.toAgentId !== 'string') throw new TypeError('messageId, fromAgentId and toAgentId are required')
      if (input.payload === undefined) throw new TypeError('message payload is required')
      requireWorker(input.fromAgentId)
      requireWorker(input.toAgentId)
      if (messages.has(input.messageId)) throw new Error(`message already exists: ${input.messageId}`)
      const message = Object.freeze({ messageId: input.messageId, fromAgentId: input.fromAgentId, toAgentId: input.toAgentId, payload: input.payload, state: 'created', createdAt: Date.now() })
      messages.set(message.messageId, message)
      persist()
      return message
    },
    transitionMessage(messageId, state) {
      const message = messages.get(messageId)
      if (!message) throw new Error(`unknown message: ${messageId}`)
      const allowed = { created: ['policy_checked', 'failed'], policy_checked: ['queued', 'failed'], queued: ['transport_accepted', 'deferred', 'failed'], transport_accepted: ['arrived', 'deferred', 'failed'], arrived: ['acknowledged', 'deferred', 'failed'], acknowledged: ['completed', 'deferred', 'failed'] }
      if (!allowed[message.state]?.includes(state)) throw new Error(`invalid message transition: ${message.state} -> ${state}`)
      const next = Object.freeze({ ...message, state, updatedAt: Date.now() })
      messages.set(messageId, next)
      persist()
      return next
    },
    acknowledgeMessage(messageId) {
      return api.transitionMessage(messageId, 'acknowledged')
    },
    listMessages: () => [...messages.values()],
    projectState: () => projectState,
    drain() {
      if (projectState !== 'accepting_workers') throw new Error(`project cannot drain from ${projectState}`)
      projectState = 'draining'
      persist()
      return projectState
    },
    closeProject(actorAgentId) {
      requirePermission(actorAgentId, 'close-project')
      if (projectState !== 'draining') throw new Error(`project cannot close from ${projectState}`)
      const openTasks = [...tasks.values()].filter((task) => !['closed', 'cancelled'].includes(task.state))
      const openMessages = [...messages.values()].filter((message) => !['completed', 'deferred', 'failed'].includes(message.state))
      if (openTasks.length || openMessages.length) throw new Error(`closeout blocked: tasks=${openTasks.length}, messages=${openMessages.length}`)
      projectState = 'closed'
      persist()
      return projectState
    },
  }

  ctx.provide('collab', api)
  return () => {}
}

CollabCore.provide = 'collab'

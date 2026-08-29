export const DashboardProjection = (ctx) => {
  const dashboard = {
    snapshot() {
      return Object.freeze({
        scope: ctx.collab.scope,
        projectState: ctx.collab.projectState(),
        workers: ctx.collab.listWorkers(),
        tasks: ctx.collab.listTasks(),
        messages: ctx.collab.listMessages(),
        deliveries: ctx.communication.listDeliveries(),
      })
    },
  }
  ctx.provide('dashboard', dashboard)
  return () => {}
}
DashboardProjection.inject = ['collab', 'communication']
DashboardProjection.provide = 'dashboard'

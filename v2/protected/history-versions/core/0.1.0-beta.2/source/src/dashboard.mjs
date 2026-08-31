export const DashboardProjection = (ctx) => {
  const dashboard = {
    snapshot() {
      return Object.freeze({
        scope: ctx.collab.scope,
        state: ctx.collab.snapshot(),
      })
    },
  }
  ctx.provide('dashboard', dashboard)
  return () => {}
}
DashboardProjection.inject = ['collab', 'communication']
DashboardProjection.provide = 'dashboard'

import { Context } from 'cordis'
import { CollabAppSdkPlugin } from './appsdk-plugin.mjs'

export async function createCollabV2(config = {}) {
  const ctx = new Context()
  await ctx.plugin(CollabAppSdkPlugin, config)
  return { ctx, collab: ctx.collab, communication: ctx.communication, dashboard: ctx.dashboard, appsdkIntegration: ctx.appsdkIntegration, dispose: () => ctx.fiber.dispose() }
}

import { CollabCore } from './collab-core.mjs'
import { CommunicationHub } from './communication.mjs'
import { DashboardProjection } from './dashboard.mjs'
import { AppSdkIntegration } from './appsdk-integration.mjs'

export async function CollabAppSdkPlugin(ctx, config = {}) {
  await ctx.plugin(CollabCore, config)
  await ctx.plugin(CommunicationHub, config)
  await ctx.plugin(DashboardProjection)
  await ctx.plugin(AppSdkIntegration, config.appsdkIntegration)
  return () => {}
}
CollabAppSdkPlugin.provide = 'collabAppSdk'

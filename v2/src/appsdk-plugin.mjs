import { CollabCore } from './collab-core.mjs'
import { CommunicationHub } from './communication.mjs'
import { DashboardProjection } from './dashboard.mjs'
import { AppSdkIntegration } from './appsdk-integration.mjs'
import { createCodexAppServerTransport } from './transports/codex-app-server.mjs'

export async function CollabAppSdkPlugin(ctx, config = {}) {
  await ctx.plugin(CollabCore, config)
  const transports = { ...(config.transports ?? {}) }
  if (config.codexAppServer) transports['codex-app-server'] = createCodexAppServerTransport(config.codexAppServer)
  await ctx.plugin(CommunicationHub, { ...config, transports })
  await ctx.plugin(DashboardProjection)
  await ctx.plugin(AppSdkIntegration, config.appsdkIntegration)
  return () => {}
}
CollabAppSdkPlugin.provide = 'collabAppSdk'

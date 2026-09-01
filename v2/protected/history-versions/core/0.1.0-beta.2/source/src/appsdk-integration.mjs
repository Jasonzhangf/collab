export const AppSdkIntegration = (ctx, config = {}) => {
  const records = []
  const integration = {
    record(kind, data) {
      if (typeof kind !== 'string' || data === undefined) throw new TypeError('integration record requires kind and data')
      const record = Object.freeze({ kind, scope: ctx.collab.scope, data, recordedAt: Date.now() })
      records.push(record)
      config.onRecord?.(record)
      return record
    },
    listRecords: () => [...records],
  }
  ctx.provide('appsdkIntegration', integration)
  return () => {}
}
AppSdkIntegration.inject = ['collab']
AppSdkIntegration.provide = 'appsdkIntegration'

import test from 'node:test'
import assert from 'node:assert/strict'
import { Context, Service } from 'cordis'

test('Cordis package provides context, service, plugin lifecycle primitives', async () => {
  assert.equal(typeof Context, 'function')
  assert.equal(typeof Service, 'function')
  const ctx = new Context()
  let disposed = false
  ctx.plugin(() => () => { disposed = true })
  await ctx.fiber.dispose()
  assert.equal(disposed, true)
})

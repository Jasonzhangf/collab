import test from 'node:test'
import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'

test('runtime manifest keeps control fields outside business payload', async () => {
  const manifest = JSON.parse(await readFile(new URL('../contracts/collab-v2-runtime.manifest.json', import.meta.url)))
  assert.equal(manifest.plugins.length, 10)
  assert.ok(manifest.transport_contract.control_fields_forbidden_in_payload.includes('presence'))
  assert.ok(manifest.transport_contract.control_fields_forbidden_in_payload.includes('retry'))
})

test('command and event contracts require typed control side-channel', async () => {
  const command = JSON.parse(await readFile(new URL('../protocol/command-v1.json', import.meta.url)))
  const event = JSON.parse(await readFile(new URL('../protocol/event-v1.json', import.meta.url)))
  assert.ok(command.required.includes('control'))
  assert.ok(command.required.includes('business_payload'))
  assert.ok(event.required.includes('correlation_id'))
  assert.ok(event.required.includes('causation_id'))
  assert.ok(event.required.includes('control'))
  assert.equal(command.properties.business_payload.additionalProperties, false)
  assert.equal(event.properties.business_payload.additionalProperties, false)
})

test('runtime module registry owns every implementation source exactly once', async () => {
  const registry = JSON.parse(await readFile(new URL('../docs/module-registry.json', import.meta.url)))
  const owned = registry.modules.flatMap((module) => module.owned_paths.map((path) => [path, module.module_id]))
  assert.equal(new Set(owned.map(([path]) => path)).size, owned.length)
  assert.ok(owned.some(([path, owner]) => path === 'crates/core/src/lib.rs' && owner === 'rust-core'))
  assert.ok(owned.some(([path, owner]) => path === 'src/collab-core.mjs' && owner === 'node-adapter'))
  assert.ok(registry.modules.every((module) => ['active', 'design', 'pending'].includes(module.status)))
})

test('active runtime maps contain no roleful task ownership semantics', async () => {
  const maps = await Promise.all(['resource-map', 'function-map', 'mainline-call-map', 'verification-map'].map(async (name) => JSON.parse(await readFile(new URL(`../docs/${name}.json`, import.meta.url)))))
  const active = JSON.stringify(maps, (_key, value) => value?.status === 'pending' || value?.status === 'design' ? undefined : value)
  for (const forbidden of ['Master', 'Worker', 'create_task', 'claim-flow', 'role-authorization', 'heartbeat']) assert.equal(active.includes(forbidden), false)
})

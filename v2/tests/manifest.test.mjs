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

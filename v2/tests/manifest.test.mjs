import test from 'node:test'
import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'

test('runtime manifest keeps control fields outside business payload', async () => {
  const manifest = JSON.parse(await readFile(new URL('../contracts/collab-v2-runtime.manifest.json', import.meta.url)))
  assert.equal(manifest.plugins.length, 10)
  assert.ok(manifest.transport_contract.control_fields_forbidden_in_payload.includes('presence'))
  assert.ok(manifest.transport_contract.control_fields_forbidden_in_payload.includes('retry'))
})

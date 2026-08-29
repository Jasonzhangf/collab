import test from 'node:test'
import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'

test('Codis container manifest limits ownership to lifecycle and health', async () => {
  const manifest = JSON.parse(await readFile(new URL('../containers/codis/runtime.json', import.meta.url)))
  assert.equal(manifest.orchestrator, 'codis')
  assert.deepEqual(manifest.owns, ['process_lifecycle', 'health_wiring'])
  assert.ok(manifest.forbidden.includes('task_truth'))
  assert.ok(manifest.forbidden.includes('claim_truth'))
})

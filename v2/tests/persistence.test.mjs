import test from 'node:test'
import assert from 'node:assert/strict'
import { mkdtemp, readFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { createCollabV2 } from '../src/index.mjs'
import { createFilePersistence } from '../src/persistence.mjs'

test('file persistence restores worker, master and task truth after restart', async () => {
  const dir = await mkdtemp(join(tmpdir(), 'collab-v2-state-'))
  const path = join(dir, 'state.json')
  const first = await createCollabV2({ cwd: '/tmp/project', persistence: createFilePersistence(path) })
  await first.collab.register({ agentId: 'persist-a', kind: 'generic', cwd: '/tmp/project', panelId: 'persist-panel', capabilities: [], endpoints: [] })
  first.collab.createTask({ taskId: 'persist-task', title: 'recover', actorAgentId: 'persist-a' })
  await first.dispose()
  const second = await createCollabV2({ cwd: '/tmp/project', persistence: createFilePersistence(path) })
  assert.equal(second.collab.whoami('persist-panel').role, 'master')
  assert.equal(second.collab.listTasks()[0].taskId, 'persist-task')
  assert.ok((await readFile(path, 'utf8')).includes('persist-task'))
  await second.dispose()
})

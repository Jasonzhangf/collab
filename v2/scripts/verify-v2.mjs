import { readFile } from 'node:fs/promises'
import { access } from 'node:fs/promises'

const manifest = JSON.parse(await readFile(new URL('../contracts/collab-v2-runtime.manifest.json', import.meta.url), 'utf8'))
const codis = JSON.parse(await readFile(new URL('../containers/codis/runtime.json', import.meta.url), 'utf8'))
if (codis.orchestrator !== 'codis' || codis.owns.join(',') !== 'process_lifecycle,health_wiring') throw new Error('invalid Codis ownership boundary')
if (manifest.lifecycle_id !== 'collab-v2-runtime') throw new Error('invalid lifecycle_id')
if (!Array.isArray(manifest.plugins) || manifest.plugins.length !== 10) throw new Error('plugin registry must contain ten plugins')
for (const plugin of manifest.plugins) {
  if (!plugin.plugin_id || !plugin.owner || !plugin.truth) throw new Error(`incomplete plugin record: ${plugin.plugin_id}`)
  await access(new URL(`../${plugin.owner}`, import.meta.url))
}
const forbidden = manifest.transport_contract.control_fields_forbidden_in_payload
if (!forbidden.includes('routing') || !forbidden.includes('error') || !forbidden.includes('scope')) throw new Error('control payload isolation contract incomplete')
console.log(`collab-v2 manifest valid: ${manifest.plugins.length} plugins`)

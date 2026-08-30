import { access, readFile, readdir } from 'node:fs/promises'
import { fileURLToPath } from 'node:url'

const manifest = JSON.parse(await readFile(new URL('../contracts/collab-v2-runtime.manifest.json', import.meta.url), 'utf8'))
const codis = JSON.parse(await readFile(new URL('../containers/codis/runtime.json', import.meta.url), 'utf8'))
const registry = JSON.parse(await readFile(new URL('../docs/module-registry.json', import.meta.url), 'utf8'))
const functionMap = JSON.parse(await readFile(new URL('../docs/function-map.json', import.meta.url), 'utf8'))
const callMap = JSON.parse(await readFile(new URL('../docs/mainline-call-map.json', import.meta.url), 'utf8'))
if (codis.orchestrator !== 'codis' || codis.owns.join(',') !== 'process_lifecycle,health_wiring') throw new Error('invalid Codis ownership boundary')
if (manifest.lifecycle_id !== 'collab-v2-runtime') throw new Error('invalid lifecycle_id')
if (!Array.isArray(manifest.plugins) || manifest.plugins.length !== 10) throw new Error('plugin registry must contain ten plugins')
for (const plugin of manifest.plugins) {
  if (!plugin.plugin_id || !plugin.owner || !plugin.truth) throw new Error(`incomplete plugin record: ${plugin.plugin_id}`)
  await access(new URL(`../${plugin.owner}`, import.meta.url))
}
const forbidden = manifest.transport_contract.control_fields_forbidden_in_payload
if (!forbidden.includes('routing') || !forbidden.includes('error') || !forbidden.includes('scope')) throw new Error('control payload isolation contract incomplete')

const ownership = new Map()
for (const module of registry.modules) {
  if (!['active', 'design', 'pending'].includes(module.status)) throw new Error(`invalid module status: ${module.module_id}`)
  for (const path of module.owned_paths) {
    if (ownership.has(path)) throw new Error(`source has multiple owners: ${path}`)
    ownership.set(path, module.module_id)
    await access(new URL(`../${path}`, import.meta.url))
  }
}
const implementationPaths = []
for (const root of registry.source_roots) {
  const rootUrl = new URL(`../${root}/`, import.meta.url)
  for (const relative of await readdir(fileURLToPath(rootUrl), { recursive: true })) {
    if (/\.(json|mjs|rs)$/.test(relative)) implementationPaths.push(`${root}/${relative}`)
  }
}
for (const path of implementationPaths) if (!ownership.has(path)) throw new Error(`source has no module owner: ${path}`)
for (const path of ownership.keys()) if (!implementationPaths.includes(path)) throw new Error(`owned path is outside source inventory: ${path}`)

for (const feature of functionMap.features.filter((entry) => entry.status === 'active')) {
  const source = (await Promise.all(feature.owned_paths.map((path) => readFile(new URL(`../${path}`, import.meta.url), 'utf8')))).join('\n')
  for (const symbol of feature.entry_symbols) {
    const terminal = symbol.split('::').at(-1)
    if (!source.includes(terminal)) throw new Error(`active symbol is not bound: ${feature.feature_id}#${symbol}`)
  }
}
for (const edge of callMap.edges.filter((entry) => entry.status === 'active')) {
  const paths = edge.path.split(' -> ')
  if (paths.length !== 2) throw new Error(`active edge is not adjacent: ${edge.edge_id}`)
  for (const path of paths) await access(new URL(`../${path}`, import.meta.url))
}

console.log(`collab-v2 architecture valid: ${manifest.plugins.length} plugins, ${ownership.size} owned sources, ${callMap.edges.filter((edge) => edge.status === 'active').length} active edges`)

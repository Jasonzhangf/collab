import { existsSync, readFileSync, writeFileSync } from 'node:fs'
import { homedir } from 'node:os'
import { resolve } from 'node:path'

export function readCodexProfile(directory = resolve(homedir(), '.codex')) {
  const activePath = resolve(directory, 'active-profile')
  if (!existsSync(activePath)) return null
  const name = readFileSync(activePath, 'utf8').trim()
  if (!name) return null
  const path = resolve(directory, `${name}.config.toml`)
  if (!existsSync(path)) return { name, path, values: {} }
  const values = {}
  for (const line of readFileSync(path, 'utf8').split('\n')) {
    const match = line.match(/^\s*(model_provider|model)\s*=\s*"([^"]+)"\s*$/)
    if (match) values[match[1]] = match[2]
  }
  return { name, path, values }
}

export function loadCollabConfig(path = resolve(homedir(), '.codex/collab.json')) {
  if (!existsSync(path)) return { active_profile: 'default', profiles: { default: {} }, path }
  const config = JSON.parse(readFileSync(path, 'utf8'))
  const active = config.active_profile ?? 'default'
  if (!config.profiles?.[active]) throw new Error(`collab profile does not exist: ${active}`)
  return { ...config, active_profile: active, path }
}

export function selectProfile(name, path = resolve(homedir(), '.codex/collab.json')) {
  const config = loadCollabConfig(path)
  if (!config.profiles?.[name]) throw new Error(`collab profile does not exist: ${name}`)
  writeFileSync(path, `${JSON.stringify({ ...config, active_profile: name, path: undefined }, (_, value) => value === undefined ? undefined : value, 2)}\n`)
  return name
}

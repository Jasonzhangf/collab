#!/usr/bin/env node
import { existsSync, mkdirSync, readFileSync, realpathSync, writeFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { createCollabV2 } from '../src/index.mjs'
import { createFilePersistence } from '../src/persistence.mjs'

const argv = process.argv.slice(2)
if (argv.includes('--help') || argv.includes('-h')) {
  process.stdout.write('Usage: collab-v2-init --cwd <project> [--upgrade] [--worker-json <json-or-path>]\n')
  process.exit(0)
}
const value = (name, fallback = null) => {
  const index = argv.indexOf(name)
  return index >= 0 ? argv[index + 1] : fallback
}
const cwd = realpathSync(resolve(value('--cwd', process.cwd())))
const controlDir = resolve(cwd, '.agent-collab-v2')
const statePath = resolve(controlDir, 'state.json')
const upgrade = argv.includes('--upgrade')
const workerJson = value('--worker-json')

if (!existsSync(cwd)) throw new Error(`cwd does not exist: ${cwd}`)
mkdirSync(controlDir, { recursive: true })
const legacyPresent = existsSync(resolve(cwd, '.agent-collab'))
const configPath = resolve(controlDir, 'config.json')
if (!existsSync(configPath)) writeFileSync(configPath, `${JSON.stringify({ schema_version: 1, cwd, scope: 'workspace', daemon: 'collab-v2', legacy: legacyPresent ? 'coexist-read-only' : 'none' }, null, 2)}\n`)

const initPromptPath = resolve(controlDir, 'INIT-AGENT.md')
const upgradePromptPath = resolve(controlDir, 'UPGRADE-AGENT.md')
const startScriptPath = resolve(controlDir, 'start.sh')
if (!existsSync(initPromptPath)) writeFileSync(initPromptPath, `# Collab initialization\n\nHuman entrypoint (one command): \`collab --cwd \"${cwd}\"\`. This initializes the workspace, starts and pings the configured App Server, and launches the configured Codex TUI.\n\nAfter startup, follow the Collab skill registration contract. Report only capabilities visible in this session. For a Codex session, report \`app-server\` only with a real thread id; the registration flow binds that thread to the verified App Server endpoint and returns outbound/inbound ping results.\n\nRegistration contract:\n\`\`\`json\n{"op":"register","worker":{"agentId":"<stable-agent-id>","kind":"codex|generic|...","cwd":"${cwd}","panelId":"<panel-or-session-id>","capabilities":[],"endpoints":[]}}\n\`\`\`\n`)
if (!existsSync(upgradePromptPath)) writeFileSync(upgradePromptPath, `# Collab v2 upgrade\n\nThis workspace may contain the legacy \`.agent-collab\` control plane. Keep legacy files and behavior read-only during migration. Initialize Collab v2 under \`.agent-collab-v2\`, report capabilities, and register once. Do not copy or reinterpret legacy tasks, messages, identities, or claims unless an explicit migration record exists. Confirm the returned v2 role and permissions before accepting work.\n`)
if (!existsSync(startScriptPath)) writeFileSync(startScriptPath, `#!/bin/sh\nexec collab --cwd ${JSON.stringify(cwd)}\n`, { mode: 0o755 })

const result = { cwd, controlDir, statePath, mode: legacyPresent || upgrade ? 'upgrade-coexistence' : 'new-install', legacyPresent, prompts: { init: initPromptPath, upgrade: upgradePromptPath }, startScript: startScriptPath, registration: 'required' }
if (workerJson) {
  const worker = workerJson.startsWith('{') ? JSON.parse(workerJson) : JSON.parse(readFileSync(resolve(cwd, workerJson), 'utf8'))
  worker.cwd = cwd
  const runtime = await createCollabV2({ cwd, persistence: createFilePersistence(statePath) })
  try {
    try {
      result.registration = await runtime.collab.register(worker)
    } catch (error) {
      result.registration = { status: 'failed', error: error.message }
    }
  } finally {
    await runtime.dispose()
  }
}
writeFileSync(resolve(controlDir, 'status.json'), `${JSON.stringify({ schema_version: 1, ...result, initializedAt: Date.now() }, null, 2)}\n`)
process.stdout.write(`${JSON.stringify(result)}\n`)

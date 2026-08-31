#!/usr/bin/env node
import { existsSync, mkdirSync, realpathSync, writeFileSync } from 'node:fs'
import { resolve } from 'node:path'

const argv = process.argv.slice(2)
if (argv.includes('--cwd') || argv.includes('--worker-json')) throw new Error('project root and identity are environment-owned')
if (argv.includes('--upgrade')) throw new Error('--upgrade is deprecated; use the formal collab migrate transaction')
if (argv.includes('--help') || argv.includes('-h')) {
  process.stdout.write('Usage: collab-v2-init (current directory only)\n')
  process.exit(0)
}
if (argv.length !== 0) throw new Error(`unknown arguments: ${argv.join(' ')}`)

const cwd = realpathSync(resolve(process.cwd()))
const controlDir = resolve(cwd, '.agent-collab-v2')
mkdirSync(controlDir, { recursive: true })
const configPath = resolve(controlDir, 'config.json')
const statusPath = resolve(controlDir, 'status.json')
const guidancePath = resolve(controlDir, 'INIT-AGENT.md')
const v1Present = existsSync(resolve(cwd, '.agent-collab'))
if (!existsSync(configPath)) writeFileSync(configPath, `${JSON.stringify({ schema_version: 2, project_root_source: 'environment', live_notification: 'tmux-only', automatic_wake: false }, null, 2)}\n`)
if (!existsSync(guidancePath)) writeFileSync(guidancePath, '# Collab v2 initialization\n\nRun `collab register` from the inherited tmux pane. Collab derives the project root, session identity, and pane endpoint from that environment. Do not provide a path, pane, role, capability list, task assignment, or wake request. Existing role-based v2 beta state must use `collab migrate inspect -> plan -> apply -> verify`; `--upgrade` is intentionally unsupported. v1 truth is not imported by this milestone.\n')
const result = { schema_version: 2, project_root: cwd, control_dir: controlDir, registration: 'required', v1_present: v1Present, migration: 'not_started' }
writeFileSync(statusPath, `${JSON.stringify(result, null, 2)}\n`)
process.stdout.write(`${JSON.stringify(result)}\n`)

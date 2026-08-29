import { closeSync, openSync, readFileSync, unlinkSync, writeFileSync } from 'node:fs'
import { resolve } from 'node:path'

export function createFilePersistence(path) {
  if (typeof path !== 'string' || path.length === 0) throw new TypeError('persistence path is required')
  return {
    statePath: path,
    load() {
      try { return JSON.parse(readFileSync(path, 'utf8')) } catch (error) {
        if (error.code === 'ENOENT') return {}
        throw error
      }
    },
    save(state) {
      writeFileSync(path, `${JSON.stringify(state)}\n`, 'utf8')
    },
    async acquireLock() {
      const lockPath = resolve(`${path}.lock`)
      for (let attempt = 0; attempt < 100; attempt += 1) {
        try {
          const fd = openSync(lockPath, 'wx', 0o600)
          return async () => { closeSync(fd); unlinkSync(lockPath) }
        } catch (error) {
          if (error.code !== 'EEXIST') throw error
          await new Promise((resolvePromise) => setTimeout(resolvePromise, 10))
        }
      }
      throw new Error(`persistence lock timeout: ${lockPath}`)
    },
  }
}

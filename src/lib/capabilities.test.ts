import mainJson from '../../src-tauri/capabilities/main.json?raw'
import { expect, it } from 'vitest'

it('grants main-window permission for every command used by the UI boundary', () => {
  const main = JSON.parse(mainJson)
  const permissions = new Set(main.permissions)
  const commands = new Set<string>()
  const sources = import.meta.glob<string>('./*.ts', { query: '?raw', import: 'default', eager: true })
  for (const [name, source] of Object.entries(sources)) {
    if (name.includes('.test.')) continue
    for (const match of source.matchAll(/invoke(?:<[^\n]*?>)?\(\s*['"]([^'"]+)['"]/g)) commands.add(match[1]!)
  }
  expect(commands.has('import_retry_playback')).toBe(true)
  const missing = [...commands].filter(command => !permissions.has(`allow-${command.replaceAll('_', '-')}`))
  expect(missing).toEqual([])
  expect(main.windows).toEqual(['main'])
})

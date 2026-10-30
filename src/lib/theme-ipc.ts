import { invoke } from '@tauri-apps/api/core'
import type { ThemeSummary } from './ipc'

export interface ThemeDocument {
  raw: string
  name: string
  colors: Record<string, string>
  css: string
  failures: ThemeSummary['failures']
}

export const themeFiles = {
  document: (id: string) => invoke<ThemeDocument>('themes_document', { id }),
  preview: (raw: string) => invoke<ThemeDocument>('themes_preview', { raw }),
  save: (raw: string) => invoke<string>('themes_save', { raw }),
  import: () => invoke<ThemeDocument | null>('themes_import'),
  export: (id: string) => invoke<boolean>('themes_export', { id }),
}

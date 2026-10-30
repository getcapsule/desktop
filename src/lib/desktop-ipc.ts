import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'

export interface DesktopSnapshot {
  close_to_tray: boolean
  output_device: string | null
  sleep_deadline_ms: number | null
}
export const desktop = {
  snapshot: () => invoke<DesktopSnapshot>('desktop_snapshot'),
  preferences: (closeToTray: boolean) => invoke<DesktopSnapshot>('desktop_preferences', { closeToTray }),
  sleep: (minutes: number | null) => invoke<DesktopSnapshot>('desktop_sleep', { minutes }),
  devices: () => invoke<string[]>('desktop_devices'),
  output: (device: string | null) => invoke<DesktopSnapshot>('desktop_output', { device }),
  subscribe: (callback: (state: DesktopSnapshot) => void) =>
    listen<DesktopSnapshot>('desktop://state', event => callback(event.payload)),
}

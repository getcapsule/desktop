export interface AudioInfo {
  generation: number
  codec?: string | null
  container?: string | null
  bitrate_kbps?: number | null
  sample_rate_hz?: number | null
  channels?: number | null
}

const valid = (n: number | null | undefined, max: number) =>
  typeof n === 'number' && Number.isFinite(n) && n > 0 && n <= max

export function acceptAudioInfo(current: AudioInfo | null, next: AudioInfo): boolean {
  return !!next && Number.isSafeInteger(next.generation) && next.generation >= (current?.generation ?? 0)
}

export function audioInfoLabel(info: AudioInfo): string {
  const parts: string[] = []
  if (info.codec || info.container) parts.push(info.codec || info.container!)
  if (valid(info.bitrate_kbps, 100000)) parts.push(`${info.bitrate_kbps} kbps`)
  if (valid(info.sample_rate_hz, 768000)) parts.push(`${Number((info.sample_rate_hz! / 1000).toFixed(3))} kHz`)
  if (valid(info.channels, 64)) parts.push(info.channels === 1 ? 'Mono' : info.channels === 2 ? 'Stereo' : `${info.channels} channels`)
  return parts.join(' · ') || 'Format unavailable'
}

export function audioInfoTooltip(info: AudioInfo): string {
  return `Source audio before output resampling. Container: ${info.container || 'unknown'}. Bitrate: ${valid(info.bitrate_kbps, 100000) ? `${info.bitrate_kbps} kbps (reported or measured average)` : 'unavailable'}.`
}

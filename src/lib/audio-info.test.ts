import { describe, expect, it } from 'vitest'
import { audioInfoLabel, audioInfoTooltip, acceptAudioInfo } from './audio-info'

describe('audio format readout', () => {
  it('labels the actual codec independently from its container', () => {
    const info = { generation: 3, codec: 'AAC', container: 'MP4', bitrate_kbps: 160, sample_rate_hz: 44100, channels: 2 }
    expect(audioInfoLabel(info)).toBe('AAC · 160 kbps · 44.1 kHz · Stereo')
    expect(audioInfoTooltip(info)).toContain('Container: MP4')
  })
  it('does not invent unavailable or invalid metadata', () => {
    expect(audioInfoLabel({ generation: 1, codec: null, container: 'MP4', bitrate_kbps: 0, sample_rate_hz: -1, channels: 0 })).toBe('MP4')
    expect(audioInfoLabel({ generation: 1 })).toBe('Format unavailable')
    expect(audioInfoLabel({ generation: 1, codec: 'FLAC', sample_rate_hz: 96000, channels: 1 })).toBe('FLAC · 96 kHz · Mono')
  })
  it('rejects snapshots from a replaced playback generation', () => {
    expect(acceptAudioInfo({ generation: 5 }, { generation: 4, codec: 'MP3' })).toBe(false)
    expect(acceptAudioInfo({ generation: 5 }, { generation: 5, codec: 'AAC' })).toBe(true)
  })
})

// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from 'vitest'
import script from '../src-tauri/resources/youtube-embed.js?raw'

function boot() {
  vi.useFakeTimers()
  document.body.innerHTML = '<p id="status"></p><div id="player"></div>'
  const invoke = vi.fn().mockResolvedValue(undefined)
  const api = {
    getVideoData: () => ({ video_id: 'abcdefghijk' }),
    getCurrentTime: () => 12.25,
    getDuration: () => 240.5,
    getPlayerState: () => 1,
    loadVideoById: vi.fn(), playVideo: vi.fn(), pauseVideo: vi.fn(),
    seekTo: vi.fn(), setVolume: vi.fn(), destroy: vi.fn(),
  }
  let config: any
  Object.assign(window, {
    __capsuleYoutubeEmbedInstance: 7,
    __TAURI_INTERNALS__: { invoke },
    YT: { Player: function (_id: string, options: any) { config = options; return api } },
  })
  new Function(script)()
  ;(window as any).onYouTubeIframeAPIReady()
  return { api, config, sent: () => invoke.mock.calls.map(([, args]) => args.event) }
}

afterEach(() => { vi.useRealTimers(); document.head.querySelectorAll('script').forEach(s => s.remove()) })

describe('experimental official YouTube embed bridge', () => {
  it('keeps official controls visible and uses its actual origin', () => {
    const { config } = boot()
    expect(config.width).toBe('100%')
    expect(config.height).toBe('100%')
    expect(config.playerVars).toMatchObject({ controls: 1, playsinline: 1, origin: window.location.origin })
    expect(config.playerVars).not.toHaveProperty('modestbranding')
  })
  it('reports player readiness and actual milliseconds from the official API', () => {
    const { api, config, sent } = boot()
    config.events.onReady({ target: api })
    expect(sent().at(-1)).toMatchObject({ instance: 7, kind: 'ready' })
    config.events.onStateChange({ data: 1, target: api })
    expect(sent().at(-1)).toEqual({ instance: 7, kind: 'state', videoId: 'abcdefghijk', state: 1, positionMs: 12250, durationMs: 240500 })
  })
  it('shows autoplay and referrer failures without reporting successful playback', () => {
    const { config, sent } = boot()
    config.events.onAutoplayBlocked({})
    expect(document.querySelector('#status')?.textContent).toContain('Click Play')
    expect(sent().at(-1).kind).toBe('autoplay-blocked')
    config.events.onError({ data: 153 })
    expect(sent().at(-1)).toMatchObject({ kind: 'error', errorCode: 153 })
    expect(document.querySelector('#status')?.textContent).toContain('153')
    expect(document.querySelector('#status')?.textContent).toContain('identification')
  })
  it('uses official transport methods and refuses invalid IDs', () => {
    const { api, config } = boot()
    config.events.onReady({ target: api })
    const send = (window as any).__capsuleYoutubeEmbedCommand
    send({ type: 'load', videoId: 'abcdefghijk' })
    expect(api.loadVideoById).toHaveBeenCalledWith({ videoId: 'abcdefghijk' })
    send({ type: 'seek', ms: 2500 }); expect(api.seekTo).toHaveBeenCalledWith(2.5, true)
    send({ type: 'volume', percent: 200 }); expect(api.setVolume).toHaveBeenCalledWith(100)
    send({ type: 'play' }); expect(api.playVideo).toHaveBeenCalled()
    send({ type: 'pause' }); expect(api.pauseVideo).toHaveBeenCalled()
    send({ type: 'load', videoId: 'https://external.example' })
    expect(api.loadVideoById).toHaveBeenCalledTimes(1)
  })
  it('ignores observations from a previous video during a requested switch', () => {
    const { api, config, sent } = boot()
    config.events.onReady({ target: api })
    ;(window as any).__capsuleYoutubeEmbedCommand({ type: 'load', videoId: '12345678901' })
    const before = sent().length
    config.events.onStateChange({ data: 1, target: api })
    expect(sent()).toHaveLength(before)
  })
  it('does not skip a duplicate occurrence on an old ended observation during reload', () => {
    const { api, config, sent } = boot()
    config.events.onReady({ target: api })
    config.events.onStateChange({ data: 0 })
    ;(window as any).__capsuleYoutubeEmbedCommand({ type: 'load', videoId: 'abcdefghijk' })
    const before = sent().length
    config.events.onStateChange({ data: 0 })
    expect(sent()).toHaveLength(before)
    config.events.onStateChange({ data: 3 })
    config.events.onStateChange({ data: 1 })
    config.events.onStateChange({ data: 0 })
    expect(sent().at(-1)).toMatchObject({ state: 0, kind: 'state' })
  })
})

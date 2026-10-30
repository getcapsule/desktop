// Experimental host for the official IFrame API. The iframe remains visible.
(() => {
  'use strict'
  const instance = window.__capsuleYoutubeEmbedInstance
  const status = document.getElementById('status')
  let player
  let ready = false
  let interval
  let requestedId
  let awaitingLoad = false
  const message = text => { if (status) status.textContent = text }
  const validId = id => typeof id === 'string' && /^[A-Za-z0-9_-]{11}$/.test(id)
  const milliseconds = seconds => Number.isFinite(seconds) && seconds >= 0 ? Math.round(seconds * 1000) : 0
  const snapshot = () => {
    const id = player?.getVideoData?.()?.video_id
    return {
      videoId: validId(id) ? id : requestedId,
      positionMs: milliseconds(player?.getCurrentTime?.()),
      durationMs: milliseconds(player?.getDuration?.()),
    }
  }
  const emit = (kind, fields = {}) => {
    const invoke = window.__TAURI_INTERNALS__?.invoke
    if (typeof invoke === 'function') {
      Promise.resolve(invoke('youtube_embed_event', { event: { instance, kind, ...fields } })).catch(() => {})
    }
  }
  const reportState = state => {
    const data = snapshot()
    // Suppress old-video observations while loadVideoById is still switching.
    if (requestedId && data.videoId !== requestedId) return
    // A repeated queue occurrence can use the same video ID. Ignore the old
    // ended state until the new load has actually started or been cued.
    if (awaitingLoad && state === 0) return
    if (state === 1 || state === 5) awaitingLoad = false
    emit('state', { ...data, state })
    if (state === 1) message('Experimental YouTube player · Playing')
    if (state === 2) message('Experimental YouTube player · Paused')
    if (state === 3) message('Experimental YouTube player · Buffering')
    if (state === 0) message('Experimental YouTube player · Ended')
  }
  const failures = {
    2: 'YouTube rejected this video ID (2).',
    5: 'YouTube could not play this video in the embedded player (5).',
    100: 'This YouTube video is unavailable or private (100).',
    101: 'The video owner does not allow embedding (101).',
    150: 'The video owner does not allow embedding (150).',
    153: 'YouTube rejected client identification or the HTTP referrer (153). This experimental local host cannot play this video.',
  }
  window.__capsuleYoutubeEmbedCommand = command => {
    if (!ready || !player || !command || typeof command !== 'object') return
    switch (command.type) {
      case 'load':
        if (!validId(command.videoId)) return
        requestedId = command.videoId
        awaitingLoad = true
        player.loadVideoById({ videoId: requestedId })
        message('Experimental YouTube player · Loading')
        break
      case 'play': player.playVideo(); break
      case 'pause': player.pauseVideo(); break
      case 'seek':
        if (Number.isFinite(command.ms) && command.ms >= 0) player.seekTo(command.ms / 1000, true)
        break
      case 'volume':
        if (Number.isFinite(command.percent)) player.setVolume(Math.min(100, Math.max(0, command.percent)))
        break
    }
  }
  window.onYouTubeIframeAPIReady = () => {
    player = new window.YT.Player('player', {
      width: '100%', height: '100%',
      playerVars: { controls: 1, playsinline: 1, origin: window.location.origin },
      events: {
        onReady: () => {
          ready = true
          message('Experimental YouTube player · Ready. Playback is unverified on this device.')
          emit('ready')
          interval = window.setInterval(() => {
            if (ready) reportState(player.getPlayerState())
          }, 1000)
        },
        onStateChange: event => reportState(event.data),
        onAutoplayBlocked: () => {
          message('Click Play in the visible YouTube player to allow playback.')
          emit('autoplay-blocked', snapshot())
        },
        onError: event => {
          const errorCode = Number(event.data)
          message(failures[errorCode] || `YouTube playback failed (${errorCode}).`)
          emit('error', { ...snapshot(), errorCode })
        },
      },
    })
  }
  const script = document.createElement('script')
  script.src = 'https://www.youtube.com/iframe_api'
  script.onerror = () => {
    message('Could not load the official YouTube player. Check your connection.')
    emit('api-error')
  }
  document.head.appendChild(script)
  window.addEventListener('pagehide', () => {
    ready = false
    window.clearInterval(interval)
    player?.destroy?.()
  }, { once: true })
})()

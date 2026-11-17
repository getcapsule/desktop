const coverSessionKey = 'capsule-home-cover-session'

function startCoverSession() {
  try {
    const previous = Number(window.localStorage.getItem(coverSessionKey) ?? '0')
    const next = Number.isSafeInteger(previous) && previous >= 0 && previous < Number.MAX_SAFE_INTEGER
      ? previous + 1
      : 0
    window.localStorage.setItem(coverSessionKey, String(next))
    return next
  } catch {
    return Math.floor(Math.random() * 1_000_000)
  }
}

export const coverSessionOffset = startCoverSession()

export function rotateCoverCandidates<T>(candidates: readonly T[], offset = coverSessionOffset): T[] {
  const start = offset % Math.max(1, candidates.length)
  return [...candidates.slice(start), ...candidates.slice(0, start)]
}

export function playlistCoverArtists(id: string, artists: string[]): string[] {
  if (artists.length < 2) return artists
  const key = `capsule-picks-cover-artist:${id}`
  let previous: string | null = null
  try { previous = window.localStorage.getItem(key) } catch { /* Storage is optional. */ }

  const choices = artists.filter(artist => artist !== previous)
  const candidates = rotateCoverCandidates(choices, Math.floor(Math.random() * choices.length))
  const first = candidates[0]
  if (first) {
    try { window.localStorage.setItem(key, first) } catch { /* Keep the current session's choice. */ }
  }
  return candidates
}

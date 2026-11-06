import type { SongRow } from './ipc'
import type { SongEntry } from './qol-ipc'

const normalize = (name: string) => name.trim().toLowerCase()
const artistKey = (song: SongRow) => normalize(song.artist_name)
const albumKey = (song: SongRow) => `${artistKey(song)}\0${normalize(song.album_name)}`
const trackKey = (song: SongRow) => `${albumKey(song)}\0${normalize(song.name)}`

export function listeningTaste(history: SongEntry[], favorites: SongEntry[]) {
  const artists = new Map<string, { name: string; score: number }>()
  const albums = new Set<string>()
  const heard = new Set<string>()
  const liked = new Set<string>()

  function add(song: SongRow, weight: number) {
    const artist = artistKey(song)
    if (artist) artists.set(artist, { name: song.artist_name, score: (artists.get(artist)?.score ?? 0) + weight })
    if (artist && song.album_name.trim()) albums.add(albumKey(song))
  }

  history.slice(0, 40).forEach(({ song }, index) => {
    heard.add(trackKey(song))
    add(song, index < 10 ? 3 : 1)
  })
  favorites.forEach(({ song }) => {
    liked.add(trackKey(song))
    add(song, 5)
  })
  return { artists, albums, heard, liked }
}

export function recommendSongs(songs: SongRow[], taste: ReturnType<typeof listeningTaste>) {
  const unique = new Map<string, SongRow>()
  for (const song of songs) {
    const key = trackKey(song)
    if (!unique.has(key)) unique.set(key, song)
  }
  const ranked = Array.from(unique, ([key, song]) => ({
    song,
    score: Math.min(16, taste.artists.get(artistKey(song))?.score ?? 0)
      + (taste.albums.has(albumKey(song)) ? 3 : 0)
      + (taste.liked.has(key) ? 2 : 0)
      - (taste.heard.has(key) ? 6 : 0),
  })).sort((a, b) => b.score - a.score)

  const counts = new Map<string, number>()
  const picks: SongRow[] = []
  const remaining: SongRow[] = []
  for (const { song } of ranked) {
    const artist = artistKey(song)
    const count = counts.get(artist) ?? 0
    if (count >= 2) remaining.push(song)
    else {
      picks.push(song)
      counts.set(artist, count + 1)
    }
    if (picks.length === 12) return picks
  }
  return [...picks, ...remaining].slice(0, 12)
}

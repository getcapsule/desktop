import type { AlbumRow, SongRow } from './ipc'
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

export function recommendSongs(songs: SongRow[], taste: ReturnType<typeof listeningTaste>, limit = 12) {
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
    if (picks.length === limit) return picks
  }
  return [...picks, ...remaining].slice(0, limit)
}

export function recommendReleases(songs: SongRow[], albums: AlbumRow[], taste: ReturnType<typeof listeningTaste>) {
  const albumsByCredit = new Map<string, AlbumRow | null>()
  const albumsByCover = new Map<string, AlbumRow | null>()
  for (const album of albums) {
    const name = normalize(album.name)
    if (!name || !album.track_count) continue
    const credit = `${normalize(album.artist_name)}\0${name}`
    albumsByCredit.set(credit, albumsByCredit.has(credit) ? null : album)
    if (album.artwork_url) {
      const cover = `${name}\0${album.artwork_url}`
      albumsByCover.set(cover, albumsByCover.has(cover) ? null : album)
    }
  }

  const releases = new Map<string, { album: AlbumRow; song: SongRow }>()
  const standalone = new Map<string, { album: undefined; song: SongRow }>()
  for (const song of recommendSongs(songs, taste, songs.length)) {
    const credit = albumsByCredit.get(albumKey(song))
    const cover = song.artwork_url ? albumsByCover.get(`${normalize(song.album_name)}\0${song.artwork_url}`) : undefined
    const album = cover ?? credit
    if (album) {
      if (!releases.has(album.id)) releases.set(album.id, { album, song })
    } else if (!standalone.has(song.id)) standalone.set(song.id, { album: undefined, song })
    if (releases.size === 12) break
  }
  return [...releases.values(), ...standalone.values()].slice(0, 12)
}

export function repeatedSongs(songs: SongRow[], history: SongEntry[]) {
  const counts = new Map<string, number>()
  for (const { song } of history) {
    const key = trackKey(song)
    counts.set(key, (counts.get(key) ?? 0) + 1)
  }
  const available = new Map(songs.map(song => [trackKey(song), song]))
  return [...counts].sort((a, b) => b[1] - a[1]).flatMap(([key, count]) => {
    const song = available.get(key)
    return count > 1 && song ? [song] : []
  }).slice(0, 12)
}

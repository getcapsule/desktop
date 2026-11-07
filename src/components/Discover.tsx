import { useContext, useEffect, useId, useMemo, useRef, useState } from 'react'
import { DownloadButton } from './Downloads'
import { NavigationIcon } from './icons'
import { ListeningContext } from './ListeningTools'
import { PlaylistDialog } from './PlaylistDialog'
import { discover, type DiscoverAlbum, type DiscoverArtist, type DiscoverTrack, type SearchKind, type SearchResults } from '../lib/discover-ipc'
import { qol } from '../lib/qol-ipc'
import { artworkUrl, formatTime, library, type AlbumRow, type SongRow } from '../lib/ipc'
import { listeningTaste, recommendReleases, recommendSongs, repeatedSongs } from '../lib/recommendations'
import { editorialPlaylists, type EditorialTrack } from '../lib/editorial-playlists'

const EMPTY: SearchResults = { tracks: [], artists: [], albums: [] }
const button = 'rounded border border-rule px-3 py-1.5 text-xs transition-colors hover:border-muted disabled:opacity-40'
const gridClass = 'grid grid-cols-[repeat(auto-fill,minmax(min(100%,11rem),1fr))] gap-3'
const cardClass = 'flex min-w-0 flex-col gap-2 rounded-lg border border-rule bg-panel/40 p-2'
const rowClass = 'row flex min-h-12 items-center gap-3 rounded px-2 py-1'
const layoutKey = 'capsule-homepage-layout'
const noAlbums: AlbumRow[] = []
type Detail = { kind: 'artist' | 'album'; id: string; title: string }
type Layout = 'list' | 'grid'

export function Discover({ albums = noAlbums, albumsLoading = false, canPlay, onPlayed }: {
  albums?: AlbumRow[]; albumsLoading?: boolean; canPlay?: (song: SongRow) => boolean; onPlayed?: () => void
} = {}) {
  const listening = useContext(ListeningContext)
  const [playlistTrack, setPlaylistTrack] = useState<DiscoverTrack | null>(null)
  const [query, setQuery] = useState('')
  const [submitted, setSubmitted] = useState('')
  const [kind, setKind] = useState<SearchKind>('tracks')
  const [layout, setLayout] = useState<Layout>(() => {
    try { return window.localStorage.getItem(layoutKey) === 'grid' ? 'grid' : 'list' }
    catch { return 'list' }
  })
  const [results, setResults] = useState<SearchResults>(EMPTY)
  const [detail, setDetail] = useState<Detail | null>(null)
  const [detailTracks, setDetailTracks] = useState<DiscoverTrack[]>([])
  const [offset, setOffset] = useState(0)
  const [detailOffset, setDetailOffset] = useState(0)
  const [loading, setLoading] = useState(false)
  const [playing, setPlaying] = useState(false)
  const [problem, setProblem] = useState<string | null>(null)
  const [notice, setNotice] = useState('')
  const generation = useRef(0)
  useEffect(() => () => { generation.current++ }, [])

  function chooseLayout(value: Layout) {
    setLayout(value)
    try { window.localStorage.setItem(layoutKey, value) } catch { /* Keep the current choice in memory. */ }
  }

  async function search(nextKind = kind, pageOffset = 0, value = query) {
    const request = ++generation.current
    const term = value.trim()
    setDetail(null); setProblem(null); setNotice(''); setOffset(pageOffset)
    setSubmitted(term); setLoading(Boolean(term))
    setResults(EMPTY)
    if (!term) return
    try {
      const found = await discover.search(nextKind, term, pageOffset)
      if (generation.current === request) setResults(found)
    } catch (error) {
      if (generation.current === request) setProblem(String(error))
    } finally {
      if (generation.current === request) setLoading(false)
    }
  }

  async function open(item: DiscoverArtist | DiscoverAlbum, type: 'artist' | 'album', pageOffset = 0) {
    const request = ++generation.current
    setDetail({ kind: type, id: item.id, title: 'name' in item ? item.name : item.title })
    setDetailOffset(pageOffset); setDetailTracks([]); setProblem(null); setNotice(''); setLoading(true)
    try {
      if (type === 'album') {
        const found = await discover.albumTracks(item.id)
        if (generation.current !== request) return
        setDetailTracks(found.tracks)
        setNotice([found.unavailable_count ? `${found.unavailable_count} unavailable tracks omitted.` : '', found.truncated ? 'This album is larger than the available track list.' : ''].filter(Boolean).join(' '))
      } else {
        const tracks = await discover.artistTracks(item.id, pageOffset)
        if (generation.current === request) setDetailTracks(tracks)
      }
    } catch (error) {
      if (generation.current === request) setProblem(String(error))
    } finally {
      if (generation.current === request) setLoading(false)
    }
  }

  const tracks = detail ? detailTracks : results.tracks
  async function play(index: number) {
    setPlaying(true); setProblem(null)
    try { await discover.play(tracks, index); onPlayed?.() }
    catch (error) { setProblem(String(error)) }
    finally { setPlaying(false) }
  }
  function back() {
    generation.current++; setDetail(null); setDetailTracks([]); setNotice(''); setProblem(null); setLoading(false)
  }
  const count = detail ? detailTracks.length : results[kind].length
  const showPicks = !submitted && !detail
  const grid = layout === 'grid'
  const pageOffset = detail ? detailOffset : offset
  const canPage = !detail || detail.kind === 'artist'
  function page(next: number) {
    if (detail) void open({ id: detail.id, name: detail.title, artwork_url: null, permalink_url: null }, 'artist', next)
    else void search(kind, next, submitted)
  }
  return <section className="flex h-full min-h-0 flex-col" aria-label="Homepage">
    <header className="shrink-0 border-b border-rule px-4 py-4">
      <div className="mb-3 flex items-center justify-between gap-3">
        <h1 className="text-base font-semibold">Homepage</h1>
        <div className="flex gap-1 rounded-md border border-rule p-0.5" role="group" aria-label="Homepage layout">
          {(['list', 'grid'] as const).map(value => <button
            key={value} type="button" aria-pressed={layout === value}
            className={`rounded px-2.5 py-1 text-xs ${layout === value ? 'bg-ink/10 text-ink' : 'text-muted hover:text-ink'}`}
            onClick={() => chooseLayout(value)}
          >{value === 'list' ? 'List' : 'Grid'}</button>)}
        </div>
      </div>
      <form role="search" onSubmit={event => { event.preventDefault(); void search() }} className="flex gap-2">
        <input type="search" aria-label="Search SoundCloud" placeholder="Search SoundCloud for songs, artists, or albums…" value={query} maxLength={512} onChange={event => setQuery(event.target.value)} className="min-w-0 flex-1 rounded border border-rule bg-ground px-3 py-2 text-sm" />
        <button type="submit" className={button}>Search</button>
      </form>
      <div className="mt-3 flex flex-wrap items-center justify-between gap-2">
        <div className="flex gap-1" role="group" aria-label="Search category">
          {(['tracks', 'artists', 'albums'] as const).map(value => <button key={value} aria-pressed={kind === value} className={`rounded px-3 py-1 text-xs ${kind === value ? 'bg-panel text-ink' : 'text-muted hover:text-ink'}`} onClick={() => { setKind(value); void search(value) }}>{value === 'tracks' ? 'Songs' : value === 'artists' ? 'Artists' : 'Albums'}</button>)}
        </div>
        {submitted && <button className={button} onClick={() => { setQuery(''); void search(kind, 0, '') }}>Back to picks</button>}
      </div>
    </header>
    {problem && <p role="alert" className="border-b border-rule px-4 py-3 text-xs text-crit">{problem}</p>}
    {detail && <div className="flex shrink-0 items-center gap-3 border-b border-rule px-4 py-3">
      <button className={button} onClick={back}>Back to results</button>
      <h2 className="min-w-0 flex-1 truncate text-sm font-semibold">{detail.title}</h2>
      <button className={button} disabled={loading || playing || !tracks.length} onClick={() => void play(0)}>{playing ? 'Starting…' : 'Play all'}</button>
      <DownloadButton label="Download all" disabled={loading} requests={tracks.map(track => ({ kind: 'soundcloud', track }))} onProblem={setProblem} />
    </div>}
    {notice && <p className="px-4 pt-3 text-xs text-muted">{notice}</p>}
    {!showPicks && <div role="status" aria-live="polite" className="shrink-0 px-4 py-2 font-mono text-[10px] text-muted">
      {loading ? 'Searching SoundCloud…' : count ? `${pageOffset + 1}–${pageOffset + count}${detail?.kind === 'album' ? ' tracks' : ' results'}` : 'No results'}
    </div>}
    <div className="min-h-0 flex-1 overflow-y-auto px-4 pb-4" aria-busy={loading}>
      {showPicks ? <HomepagePicks
        key={`${listening?.source}:${listening?.context ?? ''}`}
        grid={grid} albums={albums} albumsLoading={albumsLoading} canPlay={canPlay} onProblem={setProblem}
        onSearch={term => { setKind('tracks'); setQuery(term); void search('tracks', 0, term) }}
      /> : <div className={grid ? gridClass : undefined}>
        {!loading && (detail || kind === 'tracks') && tracks.map((track, index) => <div key={`${track.id}-${index}`} className={grid ? cardClass : rowClass}>
          <Artwork url={track.artwork_url} grid={grid} />
          <TrackLabel title={track.title} artist={track.artist} album={track.album} />
          <span className="font-mono text-[10px] text-muted">{formatTime(track.duration_ms)}</span>
          <div className="flex flex-wrap items-center gap-1.5">
            <button aria-label={`Play ${track.title}`} className={button} disabled={playing} onClick={() => void play(index)}>Play</button>
            {listening && <button aria-label={`Add ${track.title} to playlist`} aria-haspopup="dialog" className={button} onClick={() => setPlaylistTrack(track)}>Add to playlist</button>}
            <DownloadButton requests={[{ kind: 'soundcloud', track }]} onProblem={setProblem} />
          </div>
        </div>)}
        {!loading && !detail && kind === 'artists' && results.artists.map(artist => <button
          key={artist.id} className={`${grid ? cardClass : rowClass} w-full text-left`}
          onClick={() => void open(artist, 'artist')}
        >
          <Artwork url={artist.artwork_url} grid={grid} />
          <span className="min-w-0 flex-1 truncate text-sm">{artist.name}</span>
          <span className="text-xs text-muted">View songs</span>
        </button>)}
        {!loading && !detail && kind === 'albums' && results.albums.map(album => <button
          key={album.id} className={`${grid ? cardClass : rowClass} w-full text-left`}
          onClick={() => void open(album, 'album')}
        >
          <Artwork url={album.artwork_url} grid={grid} />
          <TrackLabel title={album.title} artist={album.artist} />
          <span className="text-xs text-muted">{album.track_count} tracks</span>
        </button>)}
      </div>}
    </div>
    {listening && playlistTrack && <PlaylistDialog
      description={playlistTrack.title}
      playlists={listening.snapshot.playlists}
      onSave={(id, name) => qol.playlistAddDiscovered(playlistTrack, id, name)}
      onClose={() => setPlaylistTrack(null)}
    />}
    {canPage && submitted && <footer className="flex shrink-0 justify-between border-t border-rule px-4 py-2">
      <button className={button} disabled={loading || pageOffset === 0} onClick={() => page(Math.max(0, pageOffset - 30))}>Previous</button>
      <button className={button} disabled={loading || count < 30 || pageOffset >= 9990} onClick={() => page(pageOffset + 30)}>Next</button>
    </footer>}
  </section>
}

type HomeCollection = {
  id: string; name: string; description: string; cover?: string | null; coverUpgrade?: string
} & (
  | { kind: 'album'; albumId: string }
  | { kind: 'track' | 'mix'; songs: SongRow[] }
  | { kind: 'playlist'; tracks: EditorialTrack[] }
)
type CollectionTrack = EditorialTrack & { song?: SongRow }
const capsulePicks: HomeCollection[] = editorialPlaylists.map(playlist => ({ ...playlist, kind: 'playlist' }))

function HomepagePicks({ grid, albums, albumsLoading, canPlay, onProblem, onSearch }: {
  grid: boolean; albums: AlbumRow[]; albumsLoading: boolean; canPlay?: (song: SongRow) => boolean
  onProblem: (message: string | null) => void; onSearch: (term: string) => void
}) {
  const listening = useContext(ListeningContext)
  const snapshot = listening?.snapshot
  const taste = useMemo(() => listeningTaste(snapshot?.history ?? [], snapshot?.favorites ?? []), [snapshot?.history, snapshot?.favorites])
  const artists = JSON.stringify([...taste.artists.values()].sort((a, b) => b.score - a.score).slice(0, 4).map(artist => artist.name))
  const [songs, setSongs] = useState<SongRow[] | null>(null)
  const [problem, setProblem] = useState(false)
  const [retry, setRetry] = useState(0)
  const [playing, setPlaying] = useState(false)
  const [collection, setCollection] = useState<HomeCollection | null>(null)
  const [entries, setEntries] = useState<CollectionTrack[] | null>(null)
  const [collectionError, setCollectionError] = useState('')
  const request = useRef(0)
  const backButton = useRef<HTMLButtonElement>(null)
  const cardId = useId()
  const previousCard = useRef<string | null>(null)
  const source = listening?.source
  const revision = listening?.artworkRevision
  useEffect(() => () => { request.current++ }, [])
  useEffect(() => {
    if (collection) backButton.current?.focus()
    else if (previousCard.current) {
      document.getElementById(`${cardId}-${previousCard.current}`)?.focus()
      previousCard.current = null
    }
  }, [cardId, collection])

  useEffect(() => {
    if (!source) return
    let active = true
    const names: string[] = JSON.parse(artists)
    setProblem(false)
    void Promise.all([
      ...names.map(artistName => library.songs(24, 0, { artistName })),
      library.songs(96),
    ]).then(rows => {
      if (active) setSongs(rows.flat())
    }).catch(() => { if (active) setProblem(true) })
    return () => { active = false }
  }, [artists, source, revision, retry])

  const playable = useMemo(() => {
    const candidates = songs ?? []
    return canPlay ? candidates.filter(canPlay) : candidates
  }, [songs, canPlay])
  const releases = useMemo(() => recommendReleases(playable, albums, taste), [playable, albums, taste])
  const title = taste.artists.size ? 'Picked for you' : 'From your library'
  const rotation = useMemo(() => recommendSongs(playable, taste), [playable, taste])
  const repeat = useMemo(() => repeatedSongs(playable, snapshot?.history ?? []), [playable, snapshot?.history])
  const mixes: HomeCollection[] = []
  if (taste.artists.size && rotation.length >= 2) {
    mixes.push({ kind: 'mix', id: 'rotation', name: 'Your rotation', description: 'Favorites and nearby picks from your library.', songs: rotation })
  }
  if (repeat.length >= 2) {
    mixes.push({ kind: 'mix', id: 'on-repeat', name: 'On repeat', description: 'Tracks you’ve played more than once.', songs: repeat })
  }
  const releaseCards: HomeCollection[] = releases.map(({ album, song }) => {
    const cover = album?.artwork_url ? album.id : song.artwork_url ? song.id : null
    const url = cover ? `${artworkUrl(cover, grid ? 320 : 88, revision)}&cache=only` : null
    return album
      ? {
        kind: 'album', id: `album:${album.id}`, albumId: album.id, name: album.name, description: album.artist_name, cover: url,
        coverUpgrade: cover ? `${artworkUrl(cover, 320, revision)}&quality=high` : undefined,
      }
      : { kind: 'track', id: `track:${song.id}`, name: song.name, description: song.artist_name, cover: url, songs: [song] }
  })

  async function open(item: HomeCollection) {
    const current = ++request.current
    previousCard.current = item.id
    setCollection(item); setEntries(null); setCollectionError(''); onProblem(null)
    try {
      const tracks = await collectionTracks(item, canPlay)
      if (request.current === current) setEntries(tracks)
    } catch (error) {
      if (request.current === current) setCollectionError(String(error))
    }
  }

  async function play(queue: SongRow[], index = 0) {
    setPlaying(true); onProblem(null)
    try { await library.play(queue, index) }
    catch (error) { onProblem(String(error)) }
    finally { setPlaying(false) }
  }

  if (collection) {
    const queue = entries?.flatMap(entry => entry.song ? [entry.song] : []) ?? []
    return <section aria-label={collection.name} className="space-y-3 pt-4">
      <button ref={backButton} className={button} onClick={() => { request.current++; setCollection(null); onProblem(null) }}>Back to Homepage</button>
      <div className="flex flex-wrap items-center justify-between gap-3 border-b border-rule pb-3">
        <div><h2 className="text-lg font-semibold">{collection.name}</h2><p className="text-xs text-muted">{collection.description}</p></div>
        <button className={button} disabled={!queue.length || playing} onClick={() => void play(queue)}>
          {collection.kind === 'playlist' && queue.length !== entries?.length ? 'Play cached tracks' : 'Play all'}
        </button>
      </div>
      {collectionError ? <div role="alert" className="flex flex-wrap items-center gap-3 text-xs text-crit">
        {collectionError}<button className={button} onClick={() => void open(collection)}>Retry</button>
      </div> : !entries ? <p role="status" className="text-xs text-muted">Loading tracklist…</p>
        : !entries.length ? <p className="text-xs text-muted">There are no cached tracks for this release.</p>
        : <ol>{entries.map((entry, index) => <li key={index} className={rowClass}>
          <span className="w-6 shrink-0 text-right font-mono text-[10px] text-muted">{index + 1}</span>
          <TrackLabel title={entry.name} artist={entry.artist} />
          {entry.song ? <>
            <span className="font-mono text-[10px] text-muted">{formatTime(entry.song.duration_ms)}</span>
            <button className={button} disabled={playing} aria-label={`Play ${entry.name}`}
              onClick={() => void play(queue, entries.slice(0, index).filter(row => row.song).length)}>Play</button>
          </> : collection.kind === 'playlist' ? <button className={button} aria-label={`Search SoundCloud for ${entry.name} by ${entry.artist}`}
            onClick={() => onSearch(`${entry.artist} ${entry.name}`)}>Search SoundCloud</button>
            : <span className="text-xs text-muted">Unavailable</span>}
        </li>)}</ol>}
    </section>
  }

  return <div className="space-y-6 pt-4">
    <CollectionShelf cardId={cardId} title={title} description="Picks use your library, favorites, and recent plays on this device." grid={grid} items={albumsLoading ? [] : releaseCards} onOpen={open}>
      {problem ? <div role="alert" className="flex items-center gap-3 text-xs text-muted">Couldn’t load picks from your library.<button className={button} onClick={() => setRetry(value => value + 1)}>Retry</button></div>
        : (albumsLoading || !songs && source) ? <p role="status" className="text-xs text-muted">Finding picks in your library…</p>
        : !releaseCards.length && <p className="text-xs text-muted">Add music to your library to see picks here. You can also search SoundCloud above.</p>}
    </CollectionShelf>
    {mixes.length > 0 && <CollectionShelf cardId={cardId} title="Made for you" description="Mixes assembled on this device." grid={grid} items={mixes} onOpen={open} />}
    <CollectionShelf cardId={cardId} title="Capsule Picks" description="Fixed tracklists curated by Capsule." grid={grid} items={capsulePicks} onOpen={open} />
  </div>
}

async function collectionTracks(collection: HomeCollection, canPlay?: (song: SongRow) => boolean): Promise<CollectionTrack[]> {
  if (collection.kind === 'playlist') {
    return Promise.all(collection.tracks.map(async track => {
      const results = await library.search(track.name, 100)
      const song = results.find(row => row.name.trim().toLowerCase() === track.name.toLowerCase()
        && row.artist_name.trim().toLowerCase() === track.artist.toLowerCase() && (canPlay?.(row) ?? true))
      return { ...track, song }
    }))
  }
  const songs = collection.kind === 'album' ? await library.albumSongs(collection.albumId) : collection.songs
  return songs.map(song => ({ name: song.name, artist: song.artist_name, song: (canPlay?.(song) ?? true) ? song : undefined }))
}

function CollectionShelf({ cardId, title, description, grid, items, onOpen, children }: {
  cardId: string; title: string; description: string; grid: boolean; items: HomeCollection[]
  onOpen: (item: HomeCollection) => void; children?: React.ReactNode
}) {
  return <section aria-label={title}>
    <h2 className="text-sm font-semibold">{title}</h2>
    <p className="mb-3 mt-0.5 text-xs text-muted">{description}</p>
    {children}
    <div className={grid ? gridClass : undefined}>
      {items.map(item => <button id={`${cardId}-${item.id}`} key={item.id} type="button"
        aria-label={`Open ${item.kind} ${item.name}${item.kind === 'album' ? ` by ${item.description}` : ''}`}
        className={`${grid ? cardClass : rowClass} w-full text-left hover:border-muted hover:bg-ink/5`}
        onClick={() => onOpen(item)}>
        <Artwork url={item.cover ?? null} upgradeUrl={item.coverUpgrade} grid={grid} icon={item.kind === 'album' ? 'albums' : item.kind === 'track' ? 'songs' : 'playlists'} />
        <TrackLabel title={item.name} artist={item.description} />
        <span className="shrink-0 text-xs capitalize text-muted">{item.kind}</span>
      </button>)}
    </div>
  </section>
}

function TrackLabel({ title, artist, album }: { title: string; artist: string; album?: string | null }) {
  return <span className="block min-w-0 flex-1"><span className="block truncate text-sm" title={title}>{title}</span><span className="block truncate text-xs text-muted">{artist}{album ? ` · ${album}` : ''}</span></span>
}

function Artwork({ url, upgradeUrl, grid, icon = 'songs' }: {
  url: string | null; upgradeUrl?: string; grid: boolean; icon?: 'albums' | 'songs' | 'playlists'
}) {
  const [failed, setFailed] = useState<string | null>(null)
  const frame = useRef<HTMLSpanElement>(null)
  const [visibleUpgrade, setVisibleUpgrade] = useState<string>()
  const [loadedUpgrade, setLoadedUpgrade] = useState<string>()
  useEffect(() => {
    if (!upgradeUrl || !frame.current || typeof IntersectionObserver === 'undefined') return
    let active = true
    const observer = new IntersectionObserver(entries => {
      if (active && entries.some(entry => entry.isIntersecting)) {
        setVisibleUpgrade(upgradeUrl)
        observer.disconnect()
      }
    })
    observer.observe(frame.current)
    return () => { active = false; observer.disconnect() }
  }, [upgradeUrl])
  const upgraded = upgradeUrl && loadedUpgrade === upgradeUrl
  return <span ref={frame} aria-hidden="true" className={`relative grid shrink-0 place-items-center overflow-hidden rounded bg-panel text-muted ${grid ? 'aspect-square w-full' : 'h-9 w-9'}`}>
    <NavigationIcon name={icon} size={grid ? 26 : 15} />
    {!upgraded && url && failed !== url && <img alt="" src={url} loading="lazy" referrerPolicy="no-referrer" onError={() => setFailed(url)} className="absolute inset-0 h-full w-full object-cover" />}
    {upgradeUrl && visibleUpgrade === upgradeUrl && <img alt="" src={upgradeUrl} referrerPolicy="no-referrer" onLoad={() => setLoadedUpgrade(upgradeUrl)} className={`absolute inset-0 h-full w-full object-cover ${upgraded ? '' : 'opacity-0'}`} />}
  </span>
}

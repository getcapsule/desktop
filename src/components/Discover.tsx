import { useEffect, useRef, useState } from 'react'
import { DownloadButton } from './Downloads'
import { discover, type DiscoverAlbum, type DiscoverArtist, type DiscoverTrack, type SearchKind, type SearchResults } from '../lib/discover-ipc'

const EMPTY: SearchResults = { tracks: [], artists: [], albums: [] }
const button = 'rounded border border-rule px-3 py-1.5 text-xs transition-colors hover:border-muted focus-visible:outline-2 focus-visible:outline-accent disabled:opacity-40'
type Detail = { kind: 'artist' | 'album'; id: string; title: string }

export function Discover({ onPlayed }: { onPlayed?: () => void } = {}) {
  const [query, setQuery] = useState('')
  const [submitted, setSubmitted] = useState('')
  const [kind, setKind] = useState<SearchKind>('tracks')
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
  const pageOffset = detail ? detailOffset : offset
  const canPage = !detail || detail.kind === 'artist'
  function page(next: number) {
    if (detail) void open({ id: detail.id, name: detail.title, artwork_url: null, permalink_url: null }, 'artist', next)
    else void search(kind, next, submitted)
  }
  return <section className="flex h-full min-h-0 flex-col" aria-label="Discover music">
    <header className="shrink-0 border-b border-rule px-4 py-4">
      <div className="mb-3 flex items-baseline justify-between gap-3">
        <h1 className="text-sm font-semibold">Discover</h1>
        <span className="font-mono text-[10px] text-muted">SOUNDCLOUD · PUBLIC CATALOG</span>
      </div>
      <form role="search" onSubmit={event => { event.preventDefault(); void search() }} className="flex gap-2">
        <input type="search" aria-label="Search SoundCloud" placeholder="Songs, artists, albums…" value={query} maxLength={512} onChange={event => setQuery(event.target.value)} className="min-w-0 flex-1 rounded border border-rule bg-ground px-3 py-2 text-sm outline-none focus:border-accent" />
        <button type="submit" className={button}>Search</button>
      </form>
      <div className="mt-3 flex gap-1" role="group" aria-label="Search category">
        {(['tracks', 'artists', 'albums'] as const).map(value => <button key={value} aria-pressed={kind === value} className={`rounded px-3 py-1 text-xs ${kind === value ? 'bg-panel text-ink' : 'text-muted hover:text-ink'}`} onClick={() => { setKind(value); void search(value) }}>{value === 'tracks' ? 'Songs' : value === 'artists' ? 'Artists' : 'Albums'}</button>)}
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
    <div role="status" aria-live="polite" className="shrink-0 px-4 py-2 font-mono text-[10px] text-muted">
      {loading ? 'Searching SoundCloud…' : count ? `${pageOffset + 1}–${pageOffset + count}${detail?.kind === 'album' ? ' tracks' : ' results'}` : submitted ? 'No results' : 'Search the public SoundCloud catalog'}
    </div>
    <div className="min-h-0 flex-1 overflow-y-auto px-2 pb-3" aria-busy={loading}>
      {!loading && (detail || kind === 'tracks') && tracks.map((track, index) => <div key={`${track.id}-${index}`} className="row flex min-h-12 items-center gap-3 rounded px-2 py-1">
        <Artwork url={track.artwork_url} />
        <div className="min-w-0 flex-1"><p className="truncate text-sm">{track.title}</p><p className="truncate text-xs text-muted">{track.artist}{track.album ? ` · ${track.album}` : ''}</p></div>
        <span className="font-mono text-[10px] text-muted">{Math.floor(track.duration_ms / 60000)}:{String(Math.floor(track.duration_ms / 1000) % 60).padStart(2, '0')}</span>
        <button aria-label={`Play ${track.title}`} className={button} disabled={playing} onClick={() => void play(index)}>Play</button>
        <DownloadButton requests={[{ kind: 'soundcloud', track }]} onProblem={setProblem} />
      </div>)}
      {!loading && !detail && kind === 'artists' && results.artists.map(artist => <button key={artist.id} className="row flex min-h-14 w-full items-center gap-3 rounded px-2 py-2 text-left" onClick={() => void open(artist, 'artist')}><Artwork url={artist.artwork_url} /><span className="min-w-0 flex-1 truncate text-sm">{artist.name}</span><span className="text-xs text-muted">View songs →</span></button>)}
      {!loading && !detail && kind === 'albums' && results.albums.map(album => <button key={album.id} className="row flex min-h-14 w-full items-center gap-3 rounded px-2 py-2 text-left" onClick={() => void open(album, 'album')}><Artwork url={album.artwork_url} /><span className="min-w-0 flex-1"><span className="block truncate text-sm">{album.title}</span><span className="block truncate text-xs text-muted">{album.artist}</span></span><span className="text-xs text-muted">{album.track_count} tracks →</span></button>)}
    </div>
    {canPage && submitted && <footer className="flex shrink-0 justify-between border-t border-rule px-4 py-2">
      <button className={button} disabled={loading || pageOffset === 0} onClick={() => page(Math.max(0, pageOffset - 30))}>Previous</button>
      <button className={button} disabled={loading || count < 30 || pageOffset >= 9990} onClick={() => page(pageOffset + 30)}>Next</button>
    </footer>}
  </section>
}

function Artwork({ url }: { url: string | null }) {
  return url ? <img alt="" src={url} loading="lazy" referrerPolicy="no-referrer" className="h-9 w-9 shrink-0 rounded bg-panel object-cover" /> : <span aria-hidden="true" className="flex h-9 w-9 shrink-0 items-center justify-center rounded bg-panel text-muted">♪</span>
}

import { useCallback, useEffect, useRef, useState } from 'react'
import { LIBRARY_ROW_HEIGHT, AlbumLine, Art, SongLine } from './components/LibraryRows'
import { DetailView, SavedSongs, type LibraryDetail } from './components/LibraryViews'
import { LibrarySort, SONG_SORTS, ALBUM_SORTS, ARTIST_SORTS } from './components/LibrarySort'
import { Discover } from './components/Discover'
import { PlaylistDialog, playlistButton } from './components/PlaylistDialog'
import { Downloads } from './components/Downloads'
import { useLibraryRows } from './lib/library-query'
import { ListeningContext, QueueView, SelectionBar } from './components/ListeningTools'
import { EMPTY_QOL, qol, type QolSnapshot } from './lib/qol-ipc'
import { selectionAfterClick, validNavigation, type LibraryView } from './lib/qol'
import { musicImport } from './lib/music-import'

import { CommandPalette } from './components/CommandPalette'
import { Lyrics } from './components/Lyrics'
import { NavidromeConnect } from './components/NavidromeConnect'
import { Onboarding } from './components/Onboarding'
import { Readout } from './components/Readout'
import { SettingsView } from './components/SettingsView'
import { Transport } from './components/Transport'
import { VirtualList } from './components/VirtualList'
import { WindowControls } from './components/WindowControls'
import { NavigationIcon, Search } from './components/icons'
import {
  library,
  navidrome as navidromeIpc,
  on,
  player,
  settings,
  type LibraryCounts,
  type NavidromeStatus,
  type PlayerState,
  type PlaylistRow,
  type SongRow,
  type Source,
  type SyncProgress,
  type SongQuery,
  type AlbumQuery,
  type SortDirection,
} from './lib/ipc'

function authMessage(source: Source) {
  return source === 'navidrome'
    ? 'Navidrome rejected your login. Reconnect to continue.'
    : 'Source session expired. Reconnect to continue.'
}

export default function App() {
  const [view, setView] = useState<LibraryView>('songs')
  const [listening, setListening] = useState<QolSnapshot>(EMPTY_QOL)
  const [navigationReady, setNavigationReady] = useState(false)
  const [focusTrackId, setFocusTrackId] = useState<string | null>(null)
  const [selected, setSelected] = useState<Set<number>>(new Set())
  const selectionAnchor = useRef(0)
  const searchInput = useRef<HTMLInputElement>(null)
  const [revision, setRevision] = useState(0)
  const [songSort, setSongSort] = useState<NonNullable<SongQuery['sort']>>('title')
  const [albumSort, setAlbumSort] = useState<NonNullable<AlbumQuery['sort']>>('title')
  const [detailSort, setDetailSort] = useState('original')
  const [detailDirection, setDetailDirection] = useState<SortDirection>('asc')
  const [direction, setDirection] = useState<SortDirection>('asc')
  const [artistName, setArtistName] = useState<string | null>(null)
  const [artistTab, setArtistTab] = useState<'songs' | 'albums'>('songs')
  const [playlists, setPlaylists] = useState<PlaylistRow[]>([])
  const [creatingPlaylist, setCreatingPlaylist] = useState(false)
  const [counts, setCounts] = useState<LibraryCounts | null>(null)
  const [progress, setProgress] = useState<SyncProgress | null>(null)
  const [state, setState] = useState<PlayerState | null>(null)
  const [problem, setProblem] = useState<string | null>(null)
  const [query, setQuery] = useState('')
  const [paletteOpen, setPaletteOpen] = useState(false)
  const [onboarding, setOnboarding] = useState<boolean | null>(null)
  const [source, setSource] = useState<Source>('local')
  const [ndStatus, setNdStatus] = useState<NavidromeStatus | null>(null)
  const [detail, setDetail] = useState<LibraryDetail | null>(null)
  const sourceRef = useRef<Source>('local')
  sourceRef.current = source
  const lastListRefresh = useRef(0)
  const results = query.trim() !== '' && !detail && view !== 'favorites' && view !== 'history' && view !== 'discover' && view !== 'downloads'
  const songRows = useLibraryRows(JSON.stringify([source, revision, songSort, direction, query.trim(), artistName]),
    (limit, offset) => library.songs(limit, offset, { sort: songSort, direction, query: query.trim(), ...(artistName && !results ? { artistName } : {}) }))
  const albumRows = useLibraryRows(JSON.stringify([source, revision, albumSort, direction, artistName]),
    (limit, offset) => library.albums(limit, offset, { sort: albumSort, direction, ...(artistName ? { artistName } : {}) }))
  const artistRows = useLibraryRows(JSON.stringify([source, revision, direction]),
    (limit, offset) => library.artists(limit, offset, { sort: 'name', direction }))
  const songs = songRows.rows
  const albums = albumRows.rows

  const applySource = useCallback((next: Source) => {
    sourceRef.current = next
    setSource((prev) => {
      if (prev !== next) {
        setQuery('')
        setProblem(null)
        setProgress(null)
        setDetail(null)
        setArtistName(null)
      }
      return next
    })
    if (next === 'navidrome') void navidromeIpc.status().then(setNdStatus)
    else setNdStatus(null)
  }, [])

  const reload = useCallback(async () => {
    setRevision((n) => n + 1)
    const [p, c] = await Promise.all([
      library.playlists(),
      library.counts(),
    ])
    setPlaylists(p)
    setCounts(c)
  }, [])

  useEffect(() => {
    void reload()
    void player.snapshot().then(setState)
    void settings.get().then((s) => {
      setOnboarding(!s.onboarded)
      applySource(s.source)
      return qol.snapshot()
    }).then((snapshot) => {
      if (snapshot) {
        setListening(snapshot)
        const nav = validNavigation(snapshot.navigation)
        if (nav.view) setView(nav.view)
        if (nav.songSort) setSongSort(nav.songSort)
        if (nav.albumSort) setAlbumSort(nav.albumSort)
        if (nav.direction) setDirection(nav.direction)
        if (nav.detailSort) setDetailSort(nav.detailSort)
        if (nav.detailDirection) setDetailDirection(nav.detailDirection)
        if (nav.detail !== undefined) setDetail(nav.detail)
        if (nav.artistName !== undefined) setArtistName(nav.artistName)
        if (nav.artistTab) setArtistTab(nav.artistTab)
      }
      setNavigationReady(true)
    }).catch(() => setNavigationReady(true))

    const subs = [
      on('player://state', setState),
      on('library://progress', (p) => {
        setProgress(p)
        const now = Date.now()
        if (!p.done && now - lastListRefresh.current > 1000) {
          lastListRefresh.current = now
          void reload()
        }
      }),
      on('library://updated', (c) => {
        setCounts(c)
        void reload()
        void settings.get().then((s) => applySource(s.source))
      }),
      on('library://failed', (f) => {
        setProgress(null)
        setProblem(f.needsAuth ? authMessage(sourceRef.current) : `Sync failed: ${f.reason}`)
      }),
      on('playback://error', (reason) => setProblem(`Playback failed: ${reason}`)),
      on('theme://changed', (css) => {
        document.documentElement.style.cssText = css
      }),
      on('appearance://changed', (glass) => {
        document.documentElement.dataset.glass = glass
      }),
      qol.onUpdated(snapshot => { if (snapshot) setListening(snapshot) }),
    ]
    return () => {
      for (const s of subs) void s.then((un) => un())
    }
  }, [reload])

  useEffect(() => {
    if (!navigationReady) return
    const timer = window.setTimeout(() => { void qol.navigation({ view, songSort, albumSort, direction, detail, artistName, artistTab, detailSort, detailDirection }).catch(() => {}) }, 400)
    return () => window.clearTimeout(timer)
  }, [navigationReady, view, songSort, albumSort, direction, detail, artistName, artistTab, detailSort, detailDirection])
  useEffect(() => { setSelected(new Set()); selectionAnchor.current = 0 }, [view, songSort, direction, query, artistName, detail?.id, revision, source])

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const target = e.target instanceof Element ? e.target : null
      if (target?.closest('dialog[open]')) return
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'k') {
        e.preventDefault()
        setPaletteOpen((v) => !v)
        return
      }
      if (target?.closest('input,textarea,select,button,[contenteditable="true"],[role="menu"],[cmdk-root]') || paletteOpen) return
      const mod = e.ctrlKey || e.metaKey
      if (mod && e.key.toLowerCase() === 'f') { e.preventDefault(); searchInput.current?.focus(); return }
      if (mod && e.key.toLowerCase() === 'q') { e.preventDefault(); setQuery(''); setDetail(null); setView('queue'); return }
      if (mod && e.key.toLowerCase() === 'a' && !detail && (view === 'songs' || results)) { e.preventDefault(); setSelected(new Set(songs.map((_, i) => i))); return }
      if (e.code === 'Space') { e.preventDefault(); void player.toggle().catch(err => setProblem(String(err))) }
      else if (e.altKey && e.key === 'ArrowRight') { e.preventDefault(); void player.next() }
      else if (e.altKey && e.key === 'ArrowLeft') { e.preventDefault(); void player.previous() }
      else if (mod && e.key.toLowerCase() === 'z' && view === 'queue') { e.preventDefault(); void qol.undo().catch(err => setProblem(String(err))) }
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [paletteOpen, songs, view, detail, results])

  const selectSong = (index: number, shift: boolean, toggle: boolean) => {
    setSelected(prev => selectionAfterClick(prev, index, selectionAnchor.current, shift, toggle))
    if (!shift) selectionAnchor.current = index
  }
  const openRecording = (id: string) => { setFocusTrackId(id); setQuery(''); setDetail(null); setView('settings') }
  const showAlbum = (song: SongRow) => {
    const album = albums.find(a => a.name === song.album_name && a.artist_name === song.artist_name) ?? albums.find(a => a.name === song.album_name)
    if (album) { setQuery(''); setDetail({ kind: 'album', id: album.id, title: album.name, subtitle: album.artist_name }) }
    else setProblem('This album is not cached in your library yet.')
  }

  const trackIdOf = useCallback(
    (s: SongRow) => (source === 'navidrome' || source === 'local' ? s.id : source === 'youtube' ? s.catalog_id ?? s.id : s.catalog_id),
    [source],
  )
  const canPlay = useCallback((song: SongRow) => trackIdOf(song) !== null, [trackIdOf])

  const nowPlayingId =
    state && state.index !== null ? (state.queue[state.index]?.id ?? null) : null
  const empty = counts !== null && counts.songs === 0 && counts.albums === 0 && counts.artists === 0 && counts.playlists === 0

  if (onboarding === null) {
    return <div className="flex h-full items-center justify-center text-xs text-muted" role="status">Reading settings…</div>
  }

  if (onboarding) {
    return (
      <div className="relative h-full">
        <Onboarding onFinished={() => setOnboarding(false)} />
      </div>
    )
  }

  if (source === 'navidrome' && ndStatus && !ndStatus.configured) {
    return (
      <div className="relative h-full">
        <NavidromeConnect
          initialUrl={ndStatus.url}
          initialUsername={ndStatus.username}
          onConnected={() => {
            void navidromeIpc.status().then(setNdStatus)
            void reload()
          }}
        />
      </div>
    )
  }

  return (
    <ListeningContext.Provider value={{ artworkRevision: revision, source, context: source === 'navidrome' && ndStatus ? `${ndStatus.url.replace(/\/+$/, '')}\n${ndStatus.username}` : undefined, snapshot: listening, problem: setProblem, artist: name => { setQuery(''); setDetail(null); setView('artists'); setArtistName(name); setArtistTab('songs') }, album: showAlbum, replace: openRecording }}>
    <div className="app-shell flex h-full flex-col">
      <header
        data-tauri-drag-region
        className="app-titlebar flex shrink-0 items-center gap-3"
      >
        <span data-tauri-drag-region className="app-brand">
          <span className="brand-mark" aria-hidden="true" />capsule
        </span>
        <div className="library-search">
        <Search size={16} />
        <input
          ref={searchInput}
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder={detail ? `Search this ${detail.kind}` : view === 'favorites' ? 'Search favorites' : view === 'history' ? 'Search recently played' : 'Search your library'}
          aria-label="Search your library"
          className="min-w-0 flex-1 bg-transparent text-xs text-ink outline-none placeholder:text-muted"
        />
        <button
          onClick={() => setPaletteOpen(true)}
          aria-label="Open command palette"
          className="search-shortcut shrink-0 text-muted hover:text-ink"
          title="Command palette (Ctrl+K)"
        >
          Ctrl K
        </button>
        </div>
        <span data-tauri-drag-region className="source-badge ml-auto">
          <span aria-hidden="true" />{SOURCE_NAME[source].toLowerCase()}
        </span>
        <WindowControls />
      </header>

      <div className="app-body flex min-h-0 flex-1">
        <nav aria-label="Main navigation" className="sidebar chrome flex shrink-0 flex-col gap-0.5">
          <div className="sidebar-links flex min-h-0 flex-1 flex-col gap-0.5 overflow-y-auto">
          <div className="px-4 pt-2 pb-1.5 label">
            Library
          </div>
          <Tab icon="songs" on={view === 'songs' && !results} onClick={() => { setView('songs'); setDetail(null); setArtistName(null) }}>
            Songs {counts ? <Count n={counts.songs} /> : null}
          </Tab>
          <Tab
            icon="albums"
            on={view === 'albums' && !results}
            onClick={() => {
              setDetail(null)
              setArtistName(null)
              setView('albums')
            }}
          >
            Albums {counts ? <Count n={counts.albums} /> : null}
          </Tab>
          <Tab icon="artists" on={view === 'artists' && !results} onClick={() => { setView('artists'); setArtistName(null); setDetail(null) }}>
            Artists {counts ? <Count n={counts.artists} /> : null}
          </Tab>
          <Tab
            icon="playlists"
            on={view === 'playlists' && !results}
            onClick={() => {
              setQuery('')
              setDetail(null)
              setArtistName(null)
              setView('playlists')
            }}
          >
            Playlists {counts ? <Count n={counts.playlists + listening.playlists.length} /> : null}
          </Tab>
          <div className="px-4 pt-4 pb-1.5 label">Listening</div>
          <Tab icon="favorites" on={view === 'favorites'} onClick={() => { setQuery(''); setDetail(null); setView('favorites') }}>Favorites <Count n={listening.favorites.length} /></Tab>
          <Tab icon="history" on={view === 'history'} onClick={() => { setQuery(''); setDetail(null); setView('history') }}>Recently played</Tab>
          <Tab icon="discover" on={view === 'discover'} onClick={() => { setQuery(''); setDetail(null); setView('discover') }}>Discover</Tab>
          <Tab icon="downloads" on={view === 'downloads'} onClick={() => { setQuery(''); setDetail(null); setView('downloads') }}>Downloads</Tab>
          <Tab icon="queue" on={view === 'queue' && !results} onClick={() => { setQuery(''); setDetail(null); setView('queue') }}>Queue <Count n={Math.max(0, (state?.queue.length ?? 0) - (state?.index ?? 0))} /></Tab>

          <Tab icon="lyrics" on={view === 'lyrics' && !results} onClick={() => setView('lyrics')}>
            Lyrics
          </Tab>
          </div>
          <div className="sidebar-bottom">
          <Tab icon="settings" on={view === 'settings' && !results} onClick={() => setView('settings')}>
            Settings
          </Tab>

          <div className="px-3 pt-2">
            <button
              onClick={() => source === 'youtube' ? setView('settings') : void library.sync()}
              className="sync-button w-full border border-rule px-2 py-2 text-[11px] text-muted transition-colors hover:border-accent hover:text-ink"
            >
              {progress && !progress.done ? 'Syncing...' : source === 'youtube' ? 'Import music' : 'Sync library'}
            </button>
          </div>
          </div>
        </nav>

        <main className="workspace flex min-h-0 min-w-0 flex-1 flex-col">
          {problem && (
            <div className="flex items-center gap-3 border-b border-rule/70 bg-[color-mix(in_srgb,var(--color-warn)_9%,transparent)] px-4 py-2.5 text-xs">
              <span className="text-warn">{problem}</span>
              {state?.index !== null && state?.queue[state?.index ?? -1] && <button className="rounded border border-rule px-2 py-1 text-[11px]" onClick={() => {
                const track = state.queue[state.index!]
                if (!track) return
                setProblem(null)
                void (state.imported_queue ? musicImport.retryPlayback(track.id) : player.play()).catch(e => setProblem(String(e)))
              }}>Retry playback</button>}
              {state?.imported_queue && state.index !== null && <button className="rounded border border-rule px-2 py-1 text-[11px]" onClick={() => { const track = state.queue[state.index!]; if (track) openRecording(track.id) }}>Choose another recording</button>}
              {(source === 'youtube') && <button className="rounded border border-rule px-2 py-1 text-[11px]" onClick={() => setView('settings')}>Import music</button>}
              <button aria-label="Dismiss message" className="ml-auto text-muted" onClick={() => setProblem(null)}>Dismiss</button>
            </div>
          )}

          <div className="content-surface relative flex min-h-0 flex-1 flex-col overflow-hidden">
            {!detail && (results || !artistName && ['songs', 'albums', 'artists', 'playlists'].includes(view)) && (
              <div className="workspace-heading">
                <div>
                  <p className="label">Your library</p>
                  <h1>{results ? 'Search results' : view === 'songs' ? 'Songs' : view === 'albums' ? 'Albums' : view === 'artists' ? 'Artists' : 'Playlists'}</h1>
                </div>
                {results && <span className="workspace-caption">Results for “{query.trim()}”</span>}
                {view === 'playlists' && !results && <button className={playlistButton} aria-haspopup="dialog" onClick={() => setCreatingPlaylist(true)}>New playlist</button>}
              </div>
            )}
            {creatingPlaylist && <PlaylistDialog onClose={() => setCreatingPlaylist(false)} onSave={(_, name) => qol.playlistAdd([], source, null, name)} />}
            {artistName && !detail && view === 'artists' && !results && <div className="border-b border-rule px-4 py-3">
              <button className="text-xs text-muted" onClick={() => setArtistName(null)}>Back</button>
              <h1 className="mt-2 text-sm font-semibold">{artistName}</h1>
              <p className="mt-1 text-[11px] text-muted">Songs with this exact artist metadata.</p>
              <div className="mt-3 flex gap-3 text-xs"><button onClick={() => setArtistTab('songs')}>Cached songs</button><button onClick={() => setArtistTab('albums')}>Cached albums</button></div>
            </div>}
            {!detail && (results || view === 'songs' || (view === 'artists' && artistName && artistTab === 'songs')) && <LibrarySort label="songs" options={SONG_SORTS} value={songSort} onChange={setSongSort} direction={direction} onDirection={setDirection} count={songs.length} loading={songRows.loading} />}
            {!detail && !results && (view === 'albums' || (view === 'artists' && artistName && artistTab === 'albums')) && <LibrarySort label="albums" options={ALBUM_SORTS} value={albumSort} onChange={setAlbumSort} direction={direction} onDirection={setDirection} count={albums.length} loading={albumRows.loading} />}
            {!detail && !results && view === 'artists' && !artistName && <LibrarySort label="artists" options={ARTIST_SORTS} value="name" onChange={() => {}} direction={direction} onDirection={setDirection} count={artistRows.rows.length} loading={artistRows.loading} />}
            <div className="relative min-h-0 flex-1">
            {view === 'downloads' ? (
              <Downloads />
            ) : view === 'discover' ? (
              <Discover onPlayed={() => setRevision(value => value + 1)} />
            ) : view === 'settings' && !results ? (
              <SettingsView focusTrackId={focusTrackId ?? undefined} />
            ) : view === 'queue' && !results ? (
              <QueueView state={state} onProblem={setProblem} />
            ) : (view === 'favorites' || view === 'history') && !detail ? (
              <SavedSongs entries={view === 'favorites' ? listening.favorites : listening.history} title={view === 'favorites' ? 'Favorites' : 'Recently played'} query={query} nowPlayingId={nowPlayingId} onProblem={setProblem} />
            ) : view === 'lyrics' && !results ? (
              <Lyrics state={state} />
            ) : detail && !results ? (
              <DetailView detail={detail} query={query} sort={detailSort} onSort={setDetailSort} direction={detailDirection} onDirection={setDetailDirection} localPlaylist={listening.playlists.find(p => p.id === detail.id)} canPlay={canPlay} nowPlayingId={nowPlayingId} trackIdOf={trackIdOf} onBack={() => { setDetail(null); setQuery('') }} onProblem={setProblem} />
            ) : (results || view === 'songs' || (view === 'artists' && artistName && artistTab === 'songs')) && songRows.error ? (
              <div role="alert" className="p-4 text-xs text-crit">{songRows.error}<button className="ml-3 text-ink" onClick={() => void reload()}>Retry</button></div>
            ) : empty && !progress && !listening.playlists.length && (view !== 'playlists' || results) ? (
              <Empty source={source} onSync={() => source === 'youtube' ? setView('settings') : void library.sync()} />
            ) : results || view === 'songs' || (view === 'artists' && artistName && artistTab === 'songs') ? (
              <div className="flex h-full flex-col"><SelectionBar songs={[...selected].sort((a,b) => a-b).map(i => songs[i]).filter((s): s is SongRow => !!s)} onClear={() => setSelected(new Set())} /><div className="relative min-h-0 flex-1"><VirtualList
                items={songs}
                rowHeight={LIBRARY_ROW_HEIGHT}
                empty={<span className="text-muted">{artistName ? 'No cached songs for this artist credit.' : 'No matches'}</span>}
                render={(s, i) => (
                  <SongLine
                    song={s}
                    index={i}
                    playable={canPlay(s)}
                    playing={nowPlayingId !== null && trackIdOf(s) === nowPlayingId}
                    selected={selected.has(i)}
                    onSelect={(shift, toggle) => selectSong(i, shift, toggle)}
                    onPlay={() => void library.play(songs, i)}
                  />
                )}
              /></div></div>
            ) : view === 'artists' && !artistName ? (
              <VirtualList items={artistRows.rows} rowHeight={LIBRARY_ROW_HEIGHT} empty={<span className="text-muted">No artists yet</span>} render={(artist) => <button className="row mx-2 flex h-10 w-full items-center justify-between px-2 text-left" onClick={() => { setArtistName(artist.name); setArtistTab('songs') }}><span>{artist.name}</span><span className="text-xs text-muted">{artist.song_count} cached songs · {artist.album_count} albums</span></button>} />
            ) : view === 'albums' || (view === 'artists' && artistTab === 'albums') ? (
              <VirtualList
                items={albums}
                rowHeight={LIBRARY_ROW_HEIGHT}
                empty={<span className="text-muted">No albums yet</span>}
                render={(a) => (
                  <AlbumLine
                    album={a}
                    onOpen={() => setDetail({ kind: 'album', id: a.id, title: a.name, subtitle: a.artist_name })}
                    onPlay={() => {
                      void library.playAlbum(a.id).catch((e) => setProblem(String(e)))
                    }}
                  />
                )}
              />
            ) : (
              <VirtualList
                items={[...listening.playlists.map(p => ({ id: p.id, name: p.name, artwork_url: null })), ...playlists]}
                rowHeight={LIBRARY_ROW_HEIGHT}
                empty={<span className="text-muted">No playlists yet</span>}
                render={(p) => (
                  <div
                    onClick={() => setDetail({ kind: 'playlist', id: p.id, title: p.name })}
                    onDoubleClick={() =>
                      void (p.id.startsWith('qol:') ? qol.playEntries(listening.playlists.find(list => list.id === p.id)?.songs ?? []) : library.playPlaylist(p.id)).catch((e) => setProblem(String(e)))
                    }
                    onKeyDown={(e) => {
                      if (e.key === 'Enter' || e.key === ' ') {
                        e.preventDefault()
                        setDetail({ kind: 'playlist', id: p.id, title: p.name })
                      }
                    }}
                    role="button"
                    tabIndex={0}
                    className="row mx-2 flex h-10 items-center gap-3 px-2 outline-none focus-visible:ring-1 focus-visible:ring-accent"
                    title="Open, or double-click to play"
                  >
                    <Art id={listening.playlists.find(playlist => playlist.id === p.id)?.cover_id ?? p.id} />
                    <span className="truncate">{p.name}</span>
                  </div>
                )}
              />
            )}
            </div>
          </div>
        </main>
      </div>

      <CommandPalette
        open={paletteOpen}
        onOpenChange={setPaletteOpen}
        onNavigate={next => { setQuery(''); setDetail(null); setView(next) }}
        albums={albums}
        playlists={[...listening.playlists.map(p => ({ id: p.id, name: p.name, artwork_url: null })), ...playlists]}
        localPlaylists={listening.playlists}
        canPlay={canPlay}
        onProblem={setProblem}
      />

      <Transport state={state} artworkRevision={revision} />
      <Readout
        state={state}
        counts={counts}
        progress={progress}
        source={state?.playback_source ?? source}
      />
    </div>
    </ListeningContext.Provider>
  )
}

function Tab({
  icon,
  on: active,
  onClick,
  children,
}: {
  icon: React.ComponentProps<typeof NavigationIcon>['name']
  on: boolean
  onClick: () => void
  children: React.ReactNode
}) {
  return (
    <button
      onClick={onClick}
      aria-current={active ? 'page' : undefined}
      className={`nav-item mx-2 flex items-center rounded-md px-2.5 py-1.5 text-left transition-colors ${
        active
          ? 'tab-on bg-[color-mix(in_srgb,var(--color-ink)_9%,transparent)] text-ink'
          : 'text-muted hover:bg-[color-mix(in_srgb,var(--color-ink)_5%,transparent)] hover:text-ink'
      }`}
    >
      <NavigationIcon name={icon} />
      <span className="nav-item-label">
      {children}
      </span>
    </button>
  )
}

function Count({ n }: { n: number }) {
  return <span className="data text-[10px] text-muted">{n}</span>
}

const SOURCE_NAME: Record<Source, string> = {
  soundcloud: 'SoundCloud',
  navidrome: 'Navidrome',
  youtube: 'YouTube Music',
  local: 'your folders',
}

function Empty({ source, onSync }: { source: Source; onSync: () => void }) {
  return (
    <div className="flex h-full flex-col items-center justify-center gap-3">
      <div className="text-muted">Nothing here yet.</div>
      <button
        onClick={onSync}
        className="rounded-md border border-rule px-3 py-1.5 text-xs text-muted transition-colors hover:border-muted hover:text-ink"
      >
        {source === 'youtube' ? 'Import music' : `Sync from ${SOURCE_NAME[source]}`}
      </button>
    </div>
  )
}

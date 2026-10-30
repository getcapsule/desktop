import { useEffect, useRef, useState } from 'react'
import { musicImport, type ImportJob, type ImportedCollection, type ImportUrl, type MusicPlatform, type PlaybackCandidate, type PlaylistEntry, type ImportPlaybackSettings } from '../lib/music-import'
import { artworkUrl, settings, type Settings } from '../lib/ipc'
import { SpotifySetup } from './SpotifySetup'

const buttonClass = 'rounded border border-rule px-2.5 py-1.5 text-[11px] text-muted transition-colors hover:border-accent hover:text-ink disabled:cursor-not-allowed disabled:opacity-40'
const fieldClass = 'min-w-0 flex-1 rounded border border-rule bg-ground px-3 py-2 text-[13px] text-ink placeholder:text-muted focus:border-accent focus:outline-none'
const providers: Record<MusicPlatform, string> = { spotify: 'Spotify', apple_music: 'Apple Music', youtube: 'YouTube Music', soundcloud: 'SoundCloud', navidrome: 'Navidrome', local: 'Local files' }
const statuses: Record<string, string> = { fetching: 'Fetching metadata', ready: 'Ready to import', resolving: 'Finding playback sources', completed: 'Completed', interrupted: 'Interrupted', cancelled: 'Cancelled', error: 'Import failed', pending: 'Pending', searching: 'Searching', matched: 'Matched', needs_review: 'Needs review', unavailable: 'Unavailable' }
const running = (job: ImportJob) => ['fetching', 'resolving', 'pending'].includes(job.status)
const message = (error: unknown) => error instanceof Error ? error.message : String(error)

function Action({ children, onClick, disabled = false }: { children: React.ReactNode; onClick: () => void; disabled?: boolean }) {
  return <button className={buttonClass} disabled={disabled} onClick={onClick}>{children}</button>
}

type MetadataSettingsProps = { settingsDraft?: Settings; editSettings?: (patch: Partial<Settings>) => Promise<void> }

export function ImportURL({ onComplete, settingsDraft, editSettings, focusTrackId }: { onComplete: (message: string) => void; focusTrackId?: string } & MetadataSettingsProps) {
  const [url, setUrl] = useState('')
  const [detected, setDetected] = useState<{ url: string; target: ImportUrl } | null>(null)
  const [detectError, setDetectError] = useState<string | null>(null)
  const [jobs, setJobs] = useState<ImportJob[]>([])
  const [active, setActive] = useState<ImportJob | null>(null)
  const [preview, setPreview] = useState<ImportedCollection | null>(null)
  const [entries, setEntries] = useState<PlaylistEntry[]>([])
  const [review, setReview] = useState<string | null>(null)
  const [page, setPage] = useState(0)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [revision, setRevision] = useState(0)
  const epoch = useRef(0)
  const mounted = useRef(false)
  const mutation = useRef(false)
  const latestUrl = useRef(url)
  latestUrl.current = url

  useEffect(() => {
    mounted.current = true
    void musicImport.jobs().then((recent) => {
      if (mounted.current && Array.isArray(recent)) setJobs((current) => [...current, ...recent.filter(job => !current.some(saved => saved.id === job.id))])
    }).catch(reason => { if (mounted.current) setError(message(reason)) })
    return () => { mounted.current = false; epoch.current++ }
  }, [])

  useEffect(() => {
    const value = url.trim()
    if (!value) return
    let current = true
    const timer = setTimeout(() => {
      void musicImport.detect(value).then(target => {
        if (current && latestUrl.current.trim() === value) { setDetected({ url: value, target }); setDetectError(null) }
      }).catch(reason => {
        if (current && latestUrl.current.trim() === value) { setDetected(null); setDetectError(message(reason)) }
      })
    }, 200)
    return () => { current = false; clearTimeout(timer) }
  }, [url])

  const remember = (next: ImportJob) => {
    setActive(next)
    setJobs(current => [next, ...current.filter(job => job.id !== next.id)].slice(0, 20))
  }

  useEffect(() => {
    if (!active) return
    const id = active.id
    const at = epoch.current
    let current = true
    let timer: ReturnType<typeof setTimeout>
    const valid = () => current && mounted.current && epoch.current === at
    const poll = async () => {
      try {
        const next = await musicImport.job(id)
        if (!valid()) return
        const collection = next.status !== 'fetching' ? await musicImport.preview(id) : null
        if (!valid()) return
        const rows = next.playlist_id ? await musicImport.entries(next.playlist_id) : []
        if (!valid()) return
        remember(next)
        setPreview(collection)
        setEntries(rows)
        if (running(next)) timer = setTimeout(() => void poll(), 750)
      } catch (reason) {
        if (valid()) setError(message(reason))
      }
    }
    void poll()
    return () => { current = false; clearTimeout(timer) }
  }, [active?.id, revision])

  const select = (job: ImportJob) => {
    epoch.current++
    setError(null); setPreview(null); setEntries([]); setReview(null); setPage(0)
    remember(job)
    setRevision(value => value + 1)
  }

  const perform = async (action: 'preview' | 'confirm' | 'cancel' | 'resume') => {
    if (mutation.current) return
    const selected = active
    const input = url.trim()
    if (action === 'preview' ? detected?.url !== input : !selected) return
    mutation.current = true
    setBusy(true); setError(null)
    const at = ++epoch.current
    const valid = () => mounted.current && epoch.current === at
    try {
      if (action === 'preview') {
        setActive(null); setPreview(null); setEntries([]); setReview(null); setPage(0)
        const next = await musicImport.start(input)
        if (valid()) remember(next)
      } else if (selected) {
        if (action === 'confirm') {
          const playlistId = await musicImport.confirm(selected.id)
          if (!valid()) return
          remember({ ...selected, playlist_id: playlistId, status: 'resolving' })
          setPreview(null)
          onComplete(`Saved ${selected.collection_title ?? 'import'} as a local playlist.`)
        } else {
          if (action === 'cancel') await musicImport.cancel(selected.id)
          else await musicImport.resume(selected.id)
          if (!valid()) return
          const next = await musicImport.job(selected.id)
          if (!valid()) return
          remember(next)
        }
      }
      if (valid()) setRevision(value => value + 1)
    } catch (reason) {
      if (valid()) { setError(message(reason)); setRevision(value => value + 1) }
    } finally {
      mutation.current = false
      if (valid()) setBusy(false)
    }
  }

  const refresh = () => { setError(null); setRevision(value => value + 1) }
  const displayed = entries.slice(page * 50, (page + 1) * 50)
  const previewDuplicates = preview ? preview.tracks.length - new Set(preview.tracks.map(track => `${track.origin.platform}:${track.origin.external_id}`)).size : 0

  return <div className="space-y-3">
    {focusTrackId && <div className="rounded border border-rule p-3 text-[11px] text-muted"><p className="text-ink">Choose another recording</p><TrackReview key={focusTrackId} trackId={focusTrackId} onChanged={() => { refresh(); onComplete('Playback recording updated.') }} /></div>}
    <div className="flex gap-2">
      <input aria-label="Music URL" type="url" value={url} disabled={busy} className={fieldClass}
        placeholder="Paste a playlist, album, or track URL…" onChange={event => {
          epoch.current++
          setUrl(event.target.value); setDetected(null); setDetectError(null); setError(null)
          setActive(null); setPreview(null); setEntries([]); setReview(null); setPage(0)
        }} />
      <Action onClick={() => void perform('preview')} disabled={busy || !url.trim() || detected?.url !== url.trim()}>{busy && !active ? 'Fetching…' : 'Preview'}</Action>
    </div>
    {detected?.url === url.trim() && <p className="text-[11px] text-muted">Detected provider: <span className="text-ink">{providers[detected.target.provider]}</span> · {detected.target.kind}</p>}
    {url.trim() && jobs.some(job => job.playlist_id && job.source_url === url.trim()) && <p className="text-[11px] text-warn">This URL was already imported. Saving again creates another local playlist.</p>}
    {detectError && <p role="alert" className="text-[11px] text-crit">{detectError}</p>}
    {error && <div className="space-y-2"><p role="alert" className="text-[11px] text-crit">{error}</p>{active && <Action onClick={refresh} disabled={busy}>Refresh import</Action>}</div>}

    {active && <div className="space-y-3 rounded border border-rule bg-ground p-3 text-[11px] text-muted">
      <div aria-live="polite" role="status">{statuses[active.status] ?? active.status}</div>
      {(preview?.artwork_url || active.collection_artwork) && <img src={artworkUrl(active.id, 96)} width={64} height={64} alt="Collection artwork" className="rounded" onError={event => { event.currentTarget.hidden = true }} />}
      <p className="text-xs text-ink">{preview?.title ?? active.collection_title ?? providers[active.provider as MusicPlatform] ?? 'Import'}</p>
      <p>{preview?.tracks.length ?? active.total_tracks} tracks · {active.processed} processed · {active.matched} matched · {active.needs_review} need review · {active.unavailable} unavailable</p>
      <progress aria-label="Import progress" className="w-full accent-accent" max={Math.max(1, active.total_tracks)} value={active.status === 'ready' ? active.total_tracks : active.processed} />
      {previewDuplicates > 0 && <p>{previewDuplicates} repeated occurrence{previewDuplicates === 1 ? '' : 's'} will be preserved in playlist order.</p>}
      {active.needs_review + active.unavailable > 0 && <p>Unmatched tracks stay in the playlist. Review a track below to choose its recording.</p>}
      {active.playlist_id && ['cancelled', 'interrupted', 'error'].includes(active.status) && <p>Saved tracks are kept. You can resume source checks without importing the playlist again.</p>}
      {preview?.warnings.map((warning, index) => <p key={index} className="text-warn">{warning}</p>)}
      {active.error && <p role="alert" className="text-crit">{active.error}</p>}
      <div className="flex flex-wrap gap-2">
        {active.status === 'ready' && preview && <Action disabled={busy || preview.tracks.length === 0} onClick={() => void perform('confirm')}>Save playlist</Action>}
        {(running(active) || active.status === 'ready') && <Action disabled={busy} onClick={() => void perform('cancel')}>Cancel import</Action>}
        {['interrupted', 'error', 'cancelled'].includes(active.status) && <Action disabled={busy} onClick={() => void perform('resume')}>{active.status === 'interrupted' ? 'Resume import' : 'Retry import'}</Action>}
      </div>
      {active.playlist_id && <div className="space-y-2">
        <p className="text-ink">Playlist saved. Track metadata stays in its original order while playback sources are checked.</p>
        <LocalPlaylistEditor playlistId={active.playlist_id} entries={entries} onChanged={refresh} onComplete={onComplete} />
        <ol className="max-h-72 space-y-2 overflow-y-auto">
          {displayed.map(item => <li key={item.entry_id} className="border-t border-rule pt-2">
            <div className="flex items-start gap-2"><span>{item.position + 1}.</span><div className="min-w-0 flex-1"><p className="break-words text-ink">{item.track.title}</p><p>{item.track.artists.join(', ')}</p><p>{statuses[item.resolution] ?? item.resolution}</p><TrackSource entry={item} onChanged={refresh} /></div>
              <Action onClick={() => setReview(review === item.entry_id ? null : item.entry_id)}>Review track {item.position + 1}</Action>
            </div>
            {review === item.entry_id && <TrackReview key={item.entry_id} trackId={item.track.id} onChanged={refresh} />}
          </li>)}
        </ol>
        {entries.length > 50 && <div className="flex items-center gap-2"><Action disabled={page === 0} onClick={() => setPage(value => value - 1)}>Previous tracks</Action><span>{page + 1} / {Math.ceil(entries.length / 50)}</span><Action disabled={(page + 1) * 50 >= entries.length} onClick={() => setPage(value => value + 1)}>Next tracks</Action></div>}
      </div>}
    </div>}

    {jobs.length > 0 && <details><summary className="cursor-pointer text-[11px] text-muted">Recent imports ({jobs.length})</summary><ul className="mt-2 space-y-2">{jobs.map(job => <li key={job.id} className="flex items-center justify-between gap-2 text-[11px] text-muted"><span>{job.collection_title ?? providers[job.provider as MusicPlatform] ?? 'Import'} · {statuses[job.status] ?? job.status}</span><button className={buttonClass} disabled={busy} aria-label={`Open import ${job.collection_title ?? job.id}`} onClick={() => select(job)}>Open</button></li>)}</ul></details>}
    <MetadataAccess settingsDraft={settingsDraft} editSettings={editSettings} />
    <PlaybackSources />
  </div>
}

function TrackSource({ entry, onChanged }: { entry: PlaylistEntry; onChanged: () => void }) {
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const choices = [...new Set(entry.track.origins.map(origin => origin.platform).filter(provider => provider === 'local' || provider === 'navidrome'))] as MusicPlatform[]
  if (entry.resolution === 'matched' || entry.track.origins.some(origin => origin.platform === 'youtube')) choices.push('youtube')
  if (!choices.length) return null
  return <div className="mt-1 space-y-1"><label>Playback source <select aria-label={`Playback source for entry ${entry.position + 1}`} className="rounded border border-rule bg-ground px-2 py-1 text-ink" disabled={busy} value={entry.source_preference ?? ''} onChange={event => {
    const provider = event.target.value as MusicPlatform | ''
    setBusy(true); setError(null)
    void musicImport.sourcePreference(entry.track.id, provider || null).then(onChanged).catch(reason => setError(message(reason))).finally(() => setBusy(false))
  }}><option value="">Use source priority</option>{choices.map(provider => <option key={provider} value={provider}>{providers[provider]}</option>)}</select></label>{error && <p role="alert" className="text-crit">{error}</p>}</div>
}

function LocalPlaylistEditor({ playlistId, entries, onChanged, onComplete }: { playlistId: string; entries: PlaylistEntry[]; onChanged: () => void; onComplete: (message: string) => void }) {
  const [name, setName] = useState('')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [page, setPage] = useState(0)
  const generation = useRef(0)
  useEffect(() => { generation.current++; setName(''); setError(null); setPage(0); setBusy(false); return () => { generation.current++ } }, [playlistId])
  const edit = async (operation: () => Promise<void>, note?: string) => {
    if (busy) return
    const at = generation.current
    setBusy(true); setError(null)
    try {
      await operation()
      if (at === generation.current) { if (note) onComplete(note); onChanged(); setName('') }
    } catch (reason) { if (at === generation.current) setError(message(reason)) }
    finally { if (at === generation.current) setBusy(false) }
  }
  const move = (index: number, delta: number) => {
    const next = entries.map(entry => entry.entry_id)
    const destination = index + delta
    if (destination < 0 || destination >= next.length) return
    const from = next[index], to = next[destination]
    if (from === undefined || to === undefined) return
    next[index] = to; next[destination] = from
    void edit(() => musicImport.reorder(playlistId, next))
  }
  const lastPage = Math.max(0, Math.ceil(entries.length / 50) - 1)
  const currentPage = Math.min(page, lastPage)
  return <details className="space-y-2">
    <summary className="cursor-pointer text-ink">Edit local playlist</summary>
    <p>Changes stay in Capsule and do not affect the original provider.</p>
    <div className="flex gap-2"><input aria-label="New local playlist name" className={fieldClass} value={name} onChange={event => setName(event.target.value)} disabled={busy} /><Action disabled={busy || !name.trim()} onClick={() => void edit(() => musicImport.rename(playlistId, name.trim()), 'Local playlist renamed.')}>Rename playlist</Action></div>
    {error && <p role="alert" className="text-crit">{error}</p>}
    <ol className="max-h-56 space-y-2 overflow-y-auto">{entries.slice(currentPage * 50, (currentPage + 1) * 50).map((entry, offset) => {
      const index = currentPage * 50 + offset
      return <li key={entry.entry_id} className="flex flex-wrap items-center gap-2 border-t border-rule pt-2"><span className="min-w-0 flex-1">{index + 1}. {entry.track.title}</span><Action disabled={busy || index === 0} onClick={() => move(index, -1)}>Move entry {index + 1} up</Action><Action disabled={busy || index === entries.length - 1} onClick={() => move(index, 1)}>Move entry {index + 1} down</Action><Action disabled={busy} onClick={() => void edit(() => musicImport.remove(playlistId, entry.entry_id))}>Remove entry {index + 1}</Action></li>
    })}</ol>
    {lastPage > 0 && <div className="flex gap-2"><Action disabled={currentPage === 0} onClick={() => setPage(value => value - 1)}>Previous entries</Action><span>{currentPage + 1} / {lastPage + 1}</span><Action disabled={currentPage >= lastPage} onClick={() => setPage(value => value + 1)}>Next entries</Action></div>}
  </details>
}

function PlaybackSources() {
  const [config, setConfig] = useState<ImportPlaybackSettings>({ youtube_enabled: true, priority: ['local', 'navidrome', 'youtube'] })
  const [loaded, setLoaded] = useState(false)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const mounted = useRef(false)
  const operation = useRef(0)

  const load = async () => {
    const at = ++operation.current
    setError(null); setBusy(true)
    try {
      const saved = await musicImport.playbackSettings()
      if (mounted.current && operation.current === at && saved) {
        setConfig(saved)
        setLoaded(true)
      }
    } catch (reason) {
      if (mounted.current && operation.current === at) setError(message(reason))
    } finally {
      if (mounted.current && operation.current === at) setBusy(false)
    }
  }

  useEffect(() => {
    mounted.current = true
    void load()
    return () => { mounted.current = false; operation.current++ }
  }, [])

  const persist = async (next: ImportPlaybackSettings) => {
    if (!loaded || busy) return
    const at = ++operation.current
    setBusy(true); setError(null)
    try {
      await musicImport.setPlaybackSettings(next.youtube_enabled, next.priority)
      if (mounted.current && operation.current === at) setConfig(next)
    } catch (reason) {
      if (mounted.current && operation.current === at) setError(message(reason))
    } finally {
      if (mounted.current && operation.current === at) setBusy(false)
    }
  }

  return <details>
    <summary className="cursor-pointer text-[11px] text-muted">Playback sources</summary>
    <div className="mt-3 space-y-3 text-[11px] text-muted">
      <p>Play public YouTube recordings directly. No account or cookies needed. Private or restricted recordings may be unavailable.</p>
      <label className="flex items-center gap-2">
        <input type="checkbox" checked={config.youtube_enabled} disabled={!loaded || busy} onChange={event => void persist({ ...config, youtube_enabled: event.target.checked })} />
        Enable YouTube playback
      </label>
      <label className="block">Preferred playback source
        <select value={config.priority[0] ?? 'local'} disabled={!loaded || busy} className="ml-2 rounded border border-rule bg-ground p-1 text-ink" onChange={event => {
          const first = event.target.value as MusicPlatform
          void persist({ ...config, priority: [first, ...config.priority.filter(provider => provider !== first)] })
        }}>
          <option value="local">Local files</option>
          <option value="navidrome">Navidrome</option>
          <option value="youtube" disabled={!config.youtube_enabled}>YouTube Music</option>
        </select>
      </label>
      <p>Playback tries the preferred available source first, then the remaining saved sources.</p>
      {error && <div className="space-y-2"><p role="alert" className="text-crit">{error}</p>{!loaded && <Action disabled={busy} onClick={() => void load()}>Reload playback settings</Action>}</div>}
    </div>
  </details>
}

function TrackReview({ trackId, onChanged }: { trackId: string; onChanged: () => void }) {
  const [candidates, setCandidates] = useState<PlaybackCandidate[]>([])
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const mounted = useRef(false)
  const operation = useRef(0)
  useEffect(() => {
    mounted.current = true
    const at = ++operation.current
    const valid = () => mounted.current && operation.current === at
    void musicImport.candidates(trackId).then(rows => { if (valid()) setCandidates(rows) }).catch(reason => { if (valid()) setError(message(reason)) })
    return () => { mounted.current = false; operation.current++ }
  }, [trackId])
  const run = async (candidate?: PlaybackCandidate) => {
    if (busy) return
    const at = ++operation.current
    const valid = () => mounted.current && operation.current === at
    setBusy(true); setError(null)
    try {
      if (candidate) await musicImport.choose(trackId, candidate.provider, candidate.external_id)
      else await musicImport.resolve(trackId)
      const rows = await musicImport.candidates(trackId)
      if (valid()) { setCandidates(rows); onChanged() }
    } catch (reason) { if (valid()) setError(message(reason)) }
    finally { if (valid()) setBusy(false) }
  }
  return <div className="mt-3 space-y-2 pl-4">
    <p>Choose the recording you want. Version mismatches are marked.</p>
    <Action disabled={busy} onClick={() => void run()}>Search again</Action>
    {error && <p role="alert" className="text-crit">{error}</p>}
    {candidates.length === 0 && <p>No playback candidates yet.</p>}
    {candidates.map(candidate => <div key={`${candidate.provider}:${candidate.external_id}`} className="space-y-1 border-t border-rule pt-2">
      <p className="text-ink">{candidate.title} · {candidate.artists.join(', ')}</p>
      <p>{providers[candidate.provider]} · {Math.round(candidate.confidence * 100)}% match{candidate.compatible ? '' : ' · Version mismatch'}</p>
      <Action disabled={busy} onClick={() => void run(candidate)}>{candidate.compatible ? `Use recording: ${candidate.title}` : `Use version mismatch: ${candidate.title}`}</Action>
    </div>)}
  </div>
}

const metadataCredentials = {
  spotify: { label: 'Spotify OAuth access token', help: 'Public Spotify links need no token. For optional API fallback, prefer browser sign-in above so Capsule can refresh access automatically. A manually supplied OAuth token expires.', url: 'https://developer.spotify.com/dashboard', link: 'Spotify developer dashboard' },
  apple_music: { label: 'Apple Music developer token', help: 'Public Apple Music pages import automatically. The optional official API fallback requires a developer token. Create a MusicKit key in your Apple Developer account and sign a developer token.', url: 'https://developer.apple.com/documentation/applemusicapi/generating-developer-tokens', link: 'Apple developer token guide' },
  youtube: { label: 'YouTube Data API key', help: 'Public YouTube imports work without a key. An optional YouTube Data API key can be created in Google Cloud by enabling YouTube Data API v3.', url: 'https://console.cloud.google.com/apis/library/youtube.googleapis.com', link: 'Enable YouTube Data API' },
  soundcloud: { label: 'SoundCloud OAuth access token', help: 'Public SoundCloud tracks and sets import automatically. If public access is unavailable, you can configure an optional API fallback. SoundCloud currently requires Artist Pro to register an application. Follow its authentication guide to obtain access for public metadata.', url: 'https://developers.soundcloud.com/docs/api/guide#authentication', link: 'SoundCloud authentication guide' },
} as const

function MetadataAccess({ settingsDraft, editSettings }: MetadataSettingsProps) {
  const [provider, setProvider] = useState<keyof typeof metadataCredentials>('spotify')
  const [secret, setSecret] = useState('')
  const [busy, setBusy] = useState(false)
  const [note, setNote] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [draft, setDraft] = useState<Settings | null>(null)
  const [expanded, setExpanded] = useState(false)
  const mounted = useRef(true)
  useEffect(() => { mounted.current = true; return () => { mounted.current = false } }, [])
  useEffect(() => {
    if (!expanded || settingsDraft) return
    let current = true
    void settings.get().then(next => { if (current && next) setDraft(next) }).catch(reason => { if (current) setError(message(reason)) })
    return () => { current = false }
  }, [expanded, settingsDraft])
  const editSpotify = async (patch: Partial<Settings>) => {
    if (editSettings) return editSettings(patch)
    // Re-read before saving so standalone metadata setup cannot overwrite other settings.
    const current = await settings.get()
    const next = { ...current, ...patch }
    await settings.set(next)
    if (mounted.current) setDraft(next)
  }
  const save = async () => {
    if (!secret.trim() || busy) return
    setBusy(true); setNote(null); setError(null)
    const value = secret.trim()
    setSecret('')
    try {
      await musicImport.saveCredential(provider, value)
      if (mounted.current) setNote(`${providers[provider]} metadata access saved in your credential store.`)
    } catch (reason) { if (mounted.current) setError(message(reason)) }
    finally { if (mounted.current) setBusy(false) }
  }
  const remove = async () => {
    if (busy) return
    setBusy(true); setNote(null); setError(null); setSecret('')
    try {
      await musicImport.saveCredential(provider, '')
      if (mounted.current) setNote(`${providers[provider]} manual metadata credential removed.`)
    } catch (reason) { if (mounted.current) setError(message(reason)) }
    finally { if (mounted.current) setBusy(false) }
  }
  const credential = metadataCredentials[provider]
  return <details onToggle={event => { setExpanded(event.currentTarget.open); if (!event.currentTarget.open) setSecret('') }}>
    <summary className="cursor-pointer text-[11px] text-muted">Metadata API access</summary>
    <div className="mt-3 space-y-3 text-[11px] text-muted">
      <p>Public Spotify, SoundCloud, Apple Music, and YouTube links import without account setup. Optional API access is a fallback for content the public page cannot provide. This does not change your playback source.</p>
      <label className="block">Metadata provider<select value={provider} disabled={busy} onChange={event => { setProvider(event.target.value as keyof typeof metadataCredentials); setSecret(''); setNote(null); setError(null) }} className="ml-2 rounded border border-rule bg-ground p-1 text-ink">{(['spotify', 'apple_music', 'youtube', 'soundcloud'] as const).map(id => <option key={id} value={id}>{providers[id]}</option>)}</select></label>
      {expanded && provider === 'spotify' && (settingsDraft ?? draft) && <SpotifySetup draft={(settingsDraft ?? draft)!} edit={editSpotify} />}
      <details><summary className="cursor-pointer">Advanced: manual metadata credential</summary><div className="mt-3 space-y-3">
        <p>{credential.help}</p>
        <a href={credential.url} target="_blank" rel="noopener noreferrer" className="text-accent underline underline-offset-2">{credential.link}</a>
        <label className="block">{credential.label}<input type="password" autoComplete="off" spellCheck={false} value={secret} disabled={busy} onChange={event => setSecret(event.target.value)} className={`${fieldClass} mt-1 w-full`} /></label>
        <div className="flex flex-wrap gap-2"><Action disabled={busy || !secret.trim()} onClick={() => void save()}>{busy ? 'Saving...' : 'Save metadata credential'}</Action><Action disabled={busy} onClick={() => void remove()}>Remove saved credential</Action></div>
      </div></details>
      {note && <p role="status">{note}</p>}{error && <p role="alert" className="text-crit">{error}</p>}
    </div>
  </details>
}

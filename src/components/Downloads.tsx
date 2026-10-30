import { useEffect, useState } from 'react'
import { listen } from '@tauri-apps/api/event'
import { downloads, type DownloadJob, type DownloadRequest } from '../lib/downloads-ipc'
import { chooseDownloadDirectory, downloadDirectory, queueDownloads } from '../lib/download-actions'
import { VirtualList } from './VirtualList'

const button = 'rounded border border-rule px-2.5 py-1.5 text-xs text-muted hover:border-accent hover:text-ink focus-visible:outline-2 focus-visible:outline-accent disabled:opacity-40'
export function DownloadButton({ requests, label = 'Download', disabled = false, onProblem }: { requests: DownloadRequest[]; label?: string; disabled?: boolean; onProblem?: (message: string) => void }) {
  const [busy, setBusy] = useState(false)
  const [notice, setNotice] = useState('')
  return <span className="inline-flex items-center gap-2"><button className={button} disabled={disabled || busy || !requests.length} onClick={() => {
    setBusy(true); setNotice('')
    void queueDownloads(requests).then(jobs => { if (jobs) setNotice('Queued in Downloads') }).catch(error => { setNotice(String(error)); onProblem?.(String(error)) }).finally(() => setBusy(false))
  }}>{busy ? 'Queuing…' : label}</button><span role="status" className="text-[10px] text-muted">{notice}</span></span>
}
function size(bytes: number) { return bytes >= 1024 * 1024 ? `${(bytes / 1024 / 1024).toFixed(1)} MB` : `${Math.round(bytes / 1024)} KB` }
export function Downloads() {
  const [jobs, setJobs] = useState<DownloadJob[]>([])
  const [directory, setDirectory] = useState(downloadDirectory)
  const [problem, setProblem] = useState<string | null>(null)
  useEffect(() => {
    let alive = true, revision = 0
    const subscription = listen<DownloadJob[]>('downloads://changed', event => { revision++; if (alive) setJobs(event.payload) })
    void subscription.then(async () => {
      const before = revision
      const snapshot = await downloads.snapshot()
      if (alive && before === revision) setJobs(snapshot)
      const saved = await downloads.directory()
      if (alive && saved && !downloadDirectory()) setDirectory(saved)
    }).catch(error => { if (alive) setProblem(String(error)) })
    return () => { alive = false; void subscription.then(unlisten => unlisten()).catch(() => {}) }
  }, [])
  const run = (action: Promise<unknown>) => { setProblem(null); void action.catch(error => setProblem(String(error))) }
  const active = jobs.filter(job => ['queued', 'resolving', 'downloading'].includes(job.status)).length
  return <section className="flex h-full min-h-0 flex-col" aria-label="Downloads">
    <header className="flex shrink-0 flex-wrap items-center gap-3 border-b border-rule px-4 py-4">
      <div className="mr-auto min-w-0"><h1 className="text-sm font-semibold">Downloads {active > 0 && <span className="ml-2 text-xs font-normal text-muted">{active} active</span>}</h1><p className="mt-1 max-w-lg truncate text-xs text-muted" title={directory}>{directory || 'Choose where to save your music.'}</p></div>
      <button className={button} onClick={() => run(chooseDownloadDirectory(true).then(folder => { if (folder) setDirectory(folder) }))}>{directory ? 'Change folder' : 'Choose folder'}</button>
    </header>
    {problem && <p role="alert" className="border-b border-rule px-4 py-3 text-xs text-crit">{problem}</p>}
    <p className="shrink-0 px-4 py-3 text-xs text-muted">Original audio quality. Download a song or album from Discover, or use a track’s More menu.</p>
    <div className="relative min-h-0 flex-1"><VirtualList items={jobs} rowHeight={100} empty={<span className="text-xs text-muted">Your downloads will appear here.</span>} render={job => <article className="mx-4 flex h-[100px] items-center gap-4 border-b border-rule/60 py-3">
      <div className="min-w-0 flex-1"><h2 className="truncate text-sm">{job.title}</h2><p className="mt-1 truncate text-xs text-muted">{job.artist}</p><div className="mt-2 flex items-center gap-2 text-[10px] text-muted"><span>{job.status}</span><span>{size(job.bytes)}{job.total_bytes ? ` / ${size(job.total_bytes)}` : ''}</span>{job.format && <span>{job.format.toUpperCase()}</span>}</div>{job.error && <p role="alert" className="mt-1 truncate text-[11px] text-crit" title={job.error}>{job.error}</p>}{job.status === 'downloading' && job.total_bytes && <progress aria-label={`Downloading ${job.title}`} value={Math.min(job.bytes, job.total_bytes)} max={job.total_bytes} className="mt-1 h-1 w-full accent-accent" />}</div>
      {['queued', 'resolving', 'downloading'].includes(job.status) && <button className={button} onClick={() => run(downloads.cancel(job.id))}>Cancel</button>}
      {['failed', 'cancelled'].includes(job.status) && <button className={button} onClick={() => run(downloads.retry(job.id))}>Retry</button>}
      {job.status === 'completed' && <button className={button} onClick={() => run(downloads.reveal(job.id))}>Show in folder</button>}
    </article>} /></div>
  </section>
}



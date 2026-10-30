import { Component, type ReactNode } from 'react'

export function StartupFailure({ error }: { error: unknown }) {
  const reason = error instanceof Error ? error.message : String(error)
  return <main className="flex h-full items-center justify-center bg-ground p-8 text-ink">
    <section role="alert" className="w-full max-w-lg space-y-4 rounded border border-rule bg-panel p-6">
      <p className="label text-muted">capsule</p>
      <h1 className="text-lg font-semibold">The interface couldn’t start</h1>
      <p className="text-sm text-muted">Reload to try again. If this keeps happening, include the error below in your bug report.</p>
      <pre className="max-h-48 overflow-auto whitespace-pre-wrap break-words rounded bg-ground p-3 text-xs">{reason.slice(0, 2000)}</pre>
      <button className="rounded border border-rule px-3 py-2 text-sm hover:border-accent focus-visible:outline-accent" onClick={() => window.location.reload()}>Reload capsule</button>
    </section>
  </main>
}

export class AppBoundary extends Component<{ children: ReactNode }, { error: unknown | null }> {
  state: { error: unknown | null } = { error: null }
  static getDerivedStateFromError(error: unknown) { return { error } }
  render() { return this.state.error === null ? this.props.children : <StartupFailure error={this.state.error} /> }
}

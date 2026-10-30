import { getCurrentWindow } from '@tauri-apps/api/window'
import { useEffect, useState } from 'react'

export function WindowControls() {
  const win = getCurrentWindow()
  const [maximized, setMaximized] = useState(false)

  useEffect(() => {
    let unlisten: (() => void) | undefined
    void win.isMaximized().then(setMaximized)
    void win.onResized(() => void win.isMaximized().then(setMaximized)).then((u) => {
      unlisten = u
    })
    return () => unlisten?.()
  }, [win])

  return (
    <div className="window-controls flex items-center self-start">
      <Ctl label="Minimize" onClick={() => void win.minimize()}>
        <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true">
          <line x1="1" y1="5" x2="9" y2="5" stroke="currentColor" strokeWidth="1" />
        </svg>
      </Ctl>
      <Ctl label={maximized ? 'Restore' : 'Maximize'} onClick={() => void win.toggleMaximize()}>
        {maximized ? (
          <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true">
            <rect x="1.5" y="2.5" width="5" height="5" fill="none" stroke="currentColor" strokeWidth="1" />
            <path d="M3.5 2.5 V1.5 H8.5 V6.5 H7.5" fill="none" stroke="currentColor" strokeWidth="1" />
          </svg>
        ) : (
          <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true">
            <rect x="1.5" y="1.5" width="7" height="7" fill="none" stroke="currentColor" strokeWidth="1" />
          </svg>
        )}
      </Ctl>
      <Ctl label="Close" close onClick={() => void win.close()}>
        <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true">
          <line x1="1" y1="1" x2="9" y2="9" stroke="currentColor" strokeWidth="1" />
          <line x1="9" y1="1" x2="1" y2="9" stroke="currentColor" strokeWidth="1" />
        </svg>
      </Ctl>
    </div>
  )
}

function Ctl({
  label,
  close,
  onClick,
  children,
}: {
  label: string
  close?: boolean
  onClick: () => void
  children: React.ReactNode
}) {
  return (
    <button
      aria-label={label}
      onClick={onClick}
      className={`grid h-11 w-11 place-items-center rounded-none text-muted transition-colors hover:text-ink ${
        close ? 'hover:!bg-crit hover:!text-ground' : 'hover:bg-ink/8'
      }`}
    >
      {children}
    </button>
  )
}

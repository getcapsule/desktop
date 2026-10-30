/** Public playback needs no account or browser session. */
export function YoutubeSetup() {
  return <p className="text-[12px] leading-5 text-muted">
    Ready to play. Paste a public YouTube Music track, album, or playlist URL in Import.
    No sign-in or cookies needed. Private or restricted recordings may be unavailable.
  </p>
}

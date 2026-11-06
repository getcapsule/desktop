import type { SVGProps } from 'react'

type IconProps = Omit<SVGProps<SVGSVGElement>, 'strokeWidth'> & {
  size?: number
  strokeWidth?: number
}

function Svg({ size = 16, strokeWidth = 1.6, children, ...rest }: IconProps) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={strokeWidth}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      {...rest}
    >
      {children}
    </svg>
  )
}

export function Search(p: IconProps) {
  return <Svg {...p}><circle cx="10.5" cy="10.5" r="6.5" /><path d="m16 16 4.5 4.5" /></Svg>
}

export function Volume(p: IconProps) {
  return <Svg {...p}><path d="M11 5 6 9H3v6h3l5 4V5Z M15 8a6 6 0 0 1 0 8 M18 5a10 10 0 0 1 0 14" /></Svg>
}

const navigationPaths = {
  songs: 'M9 18V5l11-2v13 M9 8l11-2 M9 18c0 1.7-1.5 3-3.5 3S2 20 2 18.5 3.5 16 5.5 16 9 16.5 9 18 M20 16c0 1.7-1.5 3-3.5 3S13 18 13 16.5 14.5 14 16.5 14 20 14.5 20 16',
  albums: 'M4 3h16v18H4z M12 8a4 4 0 1 0 0 8 4 4 0 0 0 0-8 M12 12h.01',
  artists: 'M16 7a4 4 0 1 1-8 0 4 4 0 0 1 8 0 M4 21v-2a6 6 0 0 1 6-6h4a6 6 0 0 1 6 6v2',
  playlists: 'M4 5h16 M4 10h16 M4 15h8 M17 13v8 M13 17h8',
  favorites: 'M20.8 4.6a5.5 5.5 0 0 0-7.8 0L12 5.7l-1.1-1.1a5.5 5.5 0 0 0-7.8 7.8L12 21l8.8-8.6a5.5 5.5 0 0 0 0-7.8Z',
  history: 'M3 11a9 9 0 1 1 2.6 7.4 M3 4v7h7 M12 7v5l3 2',
  home: 'm3 10 9-7 9 7v10H3z M9 20v-7h6v7',
  downloads: 'M12 3v12 M7 10l5 5 5-5 M4 16v5h16v-5',
  queue: 'M3 5h17 M3 10h17 M3 15h8 M15 14l6 4-6 4v-8Z M3 20h6',
  lyrics: 'M4 4h16v13H9l-5 4V4Z M8 8h8 M8 12h5',
  settings: 'M12 8a4 4 0 1 0 0 8 4 4 0 0 0 0-8 M9 3l-.7 2.2-2.2 1L4 5.8 2 9l1.6 1.7v2.6L2 15l2 3.2 2.1-.4 2.2 1L9 21h6l.7-2.2 2.2-1 2.1.4 2-3.2-1.6-1.7v-2.6L22 9l-2-3.2-2.1.4-2.2-1L15 3H9Z',
} as const

export function NavigationIcon({ name, ...props }: IconProps & { name: keyof typeof navigationPaths }) {
  return <Svg size={17} {...props}><path d={navigationPaths[name]} /></Svg>
}

export function Play(p: IconProps) {
  return (
    <Svg {...p}>
      <path d="M7 5 L19 12 L7 19 Z" fill="currentColor" stroke="none" />
    </Svg>
  )
}

export function Pause(p: IconProps) {
  return (
    <Svg {...p}>
      <rect x="6.5" y="5" width="3.5" height="14" fill="currentColor" stroke="none" />
      <rect x="14" y="5" width="3.5" height="14" fill="currentColor" stroke="none" />
    </Svg>
  )
}

export function Prev(p: IconProps) {
  return (
    <Svg {...p}>
      <path d="M18 5 L9 12 L18 19 Z" fill="currentColor" stroke="none" />
      <rect x="5.5" y="5" width="2.4" height="14" fill="currentColor" stroke="none" />
    </Svg>
  )
}

export function Next(p: IconProps) {
  return (
    <Svg {...p}>
      <path d="M6 5 L15 12 L6 19 Z" fill="currentColor" stroke="none" />
      <rect x="16.1" y="5" width="2.4" height="14" fill="currentColor" stroke="none" />
    </Svg>
  )
}

export function Shuffle(p: IconProps) {
  return (
    <Svg {...p}>
      <path d="M4 6 h4 l9 12 h3" />
      <path d="M17 4 l3 2 -3 2" fill="none" />
      <path d="M4 18 h4 l2.5 -3.3" />
      <path d="M14 8 l3 -2 M17 20 l3 -2 -3 -2" fill="none" />
    </Svg>
  )
}

export function Repeat(p: IconProps) {
  return (
    <Svg {...p}>
      <path d="M4 12 V9 a3 3 0 0 1 3 -3 h11" />
      <path d="M15 3 l3 3 -3 3" fill="none" />
      <path d="M20 12 v3 a3 3 0 0 1 -3 3 H6" />
      <path d="M9 21 l-3 -3 3 -3" fill="none" />
    </Svg>
  )
}

export function RepeatOne(p: IconProps) {
  return (
    <Svg {...p}>
      <path d="M4 12 V9 a3 3 0 0 1 3 -3 h11" />
      <path d="M15 3 l3 3 -3 3" fill="none" />
      <path d="M20 12 v3 a3 3 0 0 1 -3 3 H6" />
      <path d="M9 21 l-3 -3 3 -3" fill="none" />
      <path d="M11 10.5 l1.5 -1 V15" strokeWidth={1.4} />
    </Svg>
  )
}

export function Lock(p: IconProps) {
  return (
    <Svg strokeWidth={1.4} {...p}>
      <rect x="5" y="10.5" width="14" height="9" />
      <path d="M8 10.5 V7.5 a4 4 0 0 1 8 0 V10.5" fill="none" />
    </Svg>
  )
}

export function Globe(p: IconProps) {
  return (
    <Svg strokeWidth={1.4} {...p}>
      <circle cx="12" cy="12" r="8.5" />
      <path d="M3.5 12 h17 M12 3.5 c3 3 3 14 0 17 c-3 -3 -3 -14 0 -17" fill="none" />
    </Svg>
  )
}

export function Stack(p: IconProps) {
  return (
    <Svg strokeWidth={1.4} {...p}>
      <path d="M4 8 L12 4 L20 8 L12 12 Z" />
      <path d="M4 12 L12 16 L20 12" fill="none" />
      <path d="M4 16 L12 20 L20 16" fill="none" />
    </Svg>
  )
}

export function QueueList(p: IconProps) {
  return (
    <Svg strokeWidth={1.4} {...p}>
      <path d="M4 7 h10 M4 12 h10 M4 17 h6" strokeLinecap="butt" />
      <path d="M17 13 v6 l4 -3 Z" fill="currentColor" stroke="none" />
    </Svg>
  )
}

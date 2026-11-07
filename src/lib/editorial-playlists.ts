export interface EditorialTrack {
  name: string
  artist: string
}

export const editorialPlaylists = [
  {
    id: 'late-night-rnb',
    name: 'Late Night R&B',
    description: 'Unhurried R&B for the late hours.',
    tracks: [
      { name: 'Good Days', artist: 'SZA' },
      { name: 'Been Away', artist: 'Brent Faiyaz' },
      { name: 'Pink + White', artist: 'Frank Ocean' },
      { name: 'Streetcar', artist: 'Daniel Caesar' },
      { name: 'Come Over', artist: 'The Internet' },
    ],
  },
  {
    id: 'indie-after-dark',
    name: 'Indie After Dark',
    description: 'Dream pop and quiet indie with room to breathe.',
    tracks: [
      { name: 'Space Song', artist: 'Beach House' },
      { name: 'Intro', artist: 'The xx' },
      { name: 'Fade Into You', artist: 'Mazzy Star' },
      { name: 'Show Me How', artist: 'Men I Trust' },
      { name: 'Apocalypse', artist: 'Cigarettes After Sex' },
    ],
  },
] satisfies { id: string; name: string; description: string; tracks: EditorialTrack[] }[]

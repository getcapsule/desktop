export interface EditorialTrack {
  name: string
  artist: string
}

interface EditorialPlaylist {
  id: string
  name: string
  description: string
  tracks: EditorialTrack[]
}

export const editorialPlaylists: EditorialPlaylist[] = [
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
  {
    id: 'dance-floor',
    name: 'Dance Floor',
    description: 'Disco, French house, and UK garage.',
    tracks: [
      { name: 'Music Sounds Better With You', artist: 'Stardust' },
      { name: 'Finally', artist: 'CeCe Peniston' },
      { name: 'Flowers', artist: 'Sweet Female Attitude' },
      { name: 'Around the World', artist: 'Daft Punk' },
      { name: 'Lady (Hear Me Tonight)', artist: 'Modjo' },
    ],
  },
  {
    id: 'piano-and-space',
    name: 'Piano and Space',
    description: 'Instrumentals for reading and focus.',
    tracks: [
      { name: 'Avril 14th', artist: 'Aphex Twin' },
      { name: 'Ambre', artist: 'Nils Frahm' },
      { name: 'We Move Lightly', artist: 'Dustin O’Halloran' },
      { name: 'First Breath After Coma', artist: 'Explosions in the Sky' },
      { name: 'Gymnopédie No. 1', artist: 'Erik Satie' },
    ],
  },
  {
    id: 'anatolian-guitars',
    name: 'Anatolian Guitars',
    description: 'Turkish rock and psychedelic folk.',
    tracks: [
      { name: 'İnce İnce Bir Kar Yağar', artist: 'Selda Bağcan' },
      { name: 'Dönence', artist: 'Barış Manço' },
      { name: 'Resimdeki Gözyaşları', artist: 'Cem Karaca' },
      { name: 'Fesuphanallah', artist: 'Erkin Koray' },
      { name: 'Goca Dünya', artist: 'Altın Gün' },
    ],
  },
  {
    id: 'head-nod',
    name: 'Head Nod',
    description: 'Sample-heavy hip-hop across decades.',
    tracks: [
      { name: 'Electric Relaxation', artist: 'A Tribe Called Quest' },
      { name: 'Doomsday', artist: 'MF DOOM' },
      { name: 'T.R.O.Y.', artist: 'Pete Rock & C.L. Smooth' },
      { name: 'Feather', artist: 'Nujabes' },
      { name: 'The World Is Yours', artist: 'Nas' },
    ],
  },
]

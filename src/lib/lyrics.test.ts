import { describe, expect, it } from 'vitest'

import { activeItem, activeLine, gapProgress, timeline } from './lyrics'
import type { LyricLine } from './ipc'

const lines = (...ms: number[]): LyricLine[] => ms.map((at_ms, i) => ({ at_ms, text: `l${i}` }))

describe('activeLine', () => {
  const song = lines(5_000, 10_000, 20_000)

  it('has no active line during the intro', () => {
    expect(activeLine(song, 0)).toBeNull()
    expect(activeLine(song, 4_999)).toBeNull()
  })

  it('activates exactly on the timestamp', () => {
    expect(activeLine(song, 5_000)).toBe(0)
    expect(activeLine(song, 10_000)).toBe(1)
  })

  it('holds a line until the next one starts', () => {
    expect(activeLine(song, 9_999)).toBe(0)
    expect(activeLine(song, 19_999)).toBe(1)
  })

  it('holds the last line past the end', () => {
    expect(activeLine(song, 999_999)).toBe(2)
  })

  it('handles a track with no lyrics', () => {
    expect(activeLine([], 1_000)).toBeNull()
  })

  it('lands on the last of a repeated timestamp', () => {
    expect(activeLine(lines(0, 5_000, 5_000), 5_000)).toBe(2)
  })
})

const sung = (at_ms: number, text: string): LyricLine => ({ at_ms, text })

describe('timeline', () => {
  it('marks a long intro as a gap', () => {
    const items = timeline([sung(12_000, 'first')], 60_000)
    expect(items[0]).toMatchObject({ kind: 'gap', at: 0, until: 12_000 })
    expect(items[1]).toMatchObject({ kind: 'line', text: 'first' })
  })

  it('does not mark a short intro', () => {
    const items = timeline([sung(2_000, 'first')], 60_000)
    expect(items[0]?.kind).toBe('line')
  })

  it('turns a long empty line into a gap and drops short ones', () => {
    const items = timeline(
      [sung(0, 'a'), sung(1_000, ''), sung(2_000, 'b'), sung(3_000, ''), sung(30_000, 'c')],
      60_000,
    )
    expect(items.map((i) => i.kind)).toEqual(['line', 'line', 'gap', 'line', 'gap'])
  })

  it('adds an outro when the last line is followed by a long tail', () => {
    const items = timeline([sung(0, 'only')], 60_000)
    expect(items[items.length - 1]?.kind).toBe('gap')
  })

  it('adds no outro when the track ends with the last line', () => {
    const items = timeline([sung(0, 'only')], 1_000)
    expect(items.every((i) => i.kind === 'line')).toBe(true)
  })

  it('is empty when there are no lyrics', () => {
    expect(timeline([], 60_000)).toEqual([])
  })
})

describe('activeItem and gapProgress', () => {
  const items = timeline([sung(10_000, 'a'), sung(20_000, 'b')], 25_000)

  it('selects the covering item', () => {
    expect(activeItem(items, 0)).toBe(0) // intro gap
    expect(activeItem(items, 10_000)).toBe(1)
    expect(activeItem(items, 21_000)).toBe(2)
  })

  it('reports progress across a gap and clamps at both ends', () => {
    const intro = items[0]
    expect(intro).toBeDefined()
    if (!intro) return
    expect(gapProgress(intro, 0)).toBe(0)
    expect(gapProgress(intro, 5_000)).toBeCloseTo(0.5)
    expect(gapProgress(intro, 99_000)).toBe(1)
    expect(gapProgress(intro, -5_000)).toBe(0)
  })
})

it('keeps the outro gap open until the actual end of the track', () => {
  const items = timeline([sung(0, 'only')], 60000)
  expect(items.at(-1)).toEqual({ kind: 'gap', at: 5000, until: 60000 })
})

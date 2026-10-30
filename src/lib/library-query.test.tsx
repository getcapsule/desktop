// @vitest-environment jsdom
import { act, cleanup, renderHook, waitFor } from '@testing-library/react'
import { afterEach, expect, it, vi } from 'vitest'
import { fetchLibraryRows, useLibraryRows } from './library-query'

afterEach(cleanup)

it('fetches every SQL-ordered page beyond 500 without changing order', async () => {
  const rows = Array.from({ length: 1007 }, (_, i) => ({ id: `track-${i}` }))
  const page = vi.fn(async (limit: number, offset: number) => rows.slice(offset, offset + limit))
  expect(await fetchLibraryRows(page)).toEqual(rows)
  expect(page.mock.calls).toEqual([[500, 0], [500, 500], [500, 1000]])
})

it('stops paging when the source or query is cancelled', async () => {
  let active = true
  const page = vi.fn(async () => { active = false; return Array.from({ length: 500 }, (_, i) => i) })
  await fetchLibraryRows(page, () => active)
  expect(page).toHaveBeenCalledTimes(1)
})

it('does not return a partial queue when a later page fails', async () => {
  const page = vi.fn(async (_limit: number, offset: number) => {
    if (offset > 0) throw new Error('Network lost')
    return Array.from({ length: 500 }, (_, i) => i)
  })
  await expect(fetchLibraryRows(page)).rejects.toThrow('Network lost')
})

it('discards stale source and sort results even when they finish last', async () => {
  let finish!: (rows: string[]) => void
  const old = new Promise<string[]>((resolve) => { finish = resolve })
  const { result, rerender } = renderHook(({ key }) => useLibraryRows(key, async () => key === 'old' ? old : ['new']), { initialProps: { key: 'old' } })
  rerender({ key: 'new' })
  await waitFor(() => expect(result.current.rows).toEqual(['new']))
  await act(async () => finish(['stale']))
  expect(result.current.rows).toEqual(['new'])
})

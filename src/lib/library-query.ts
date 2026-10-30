import { useEffect, useRef, useState } from 'react'

const PAGE_SIZE = 500

/** Fetch a complete, database-sorted query so row indices also address its queue. */
export async function fetchLibraryRows<T>(page: (limit: number, offset: number) => Promise<T[]>, active = () => true): Promise<T[]> {
  const rows: T[] = []
  for (let offset = 0; active(); offset += PAGE_SIZE) {
    const next = await page(PAGE_SIZE, offset)
    if (!active()) return []
    rows.push(...next)
    if (next.length < PAGE_SIZE) break
  }
  return rows
}

/** The key must include source, refresh revision, and every query/sort option. */
export function useLibraryRows<T>(key: string, page: (limit: number, offset: number) => Promise<T[]>) {
  const fetchPage = useRef(page)
  fetchPage.current = page
  const [state, setState] = useState<{ key: string; rows: T[]; loading: boolean; error: string | null }>({ key, rows: [], loading: true, error: null })
  useEffect(() => {
    let active = true
    const currentPage = fetchPage.current
    setState({ key, rows: [], loading: true, error: null })
    void fetchLibraryRows(currentPage, () => active).then((rows) => {
      if (active) setState({ key, rows, loading: false, error: null })
    }).catch((reason) => {
      if (active) setState({ key, rows: [], loading: false, error: String(reason) })
    })
    return () => { active = false }
  }, [key])
  // Do not expose a previous source/query during the render before effect cleanup.
  return state.key === key ? state : { key, rows: [] as T[], loading: true, error: null }
}

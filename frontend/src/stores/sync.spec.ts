import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createPinia, setActivePinia } from 'pinia'
import { useSyncStore } from './sync'

// sync storeのfetchHealth状態遷移(TASK-9設計7章・12章P2のliveness表示)と
// 手動refresh(TASK-9設計4章D10・コードレビュー指摘高4)。refresh分は実装前の
// ためpostSyncRefresh(client)・refresh action未実装=該当testは実行failを許容
// (実装後に緑化)。

const syncBodyOf = (status: string, entry_count: number) => ({
  scope: 'api-bucket:amazing-backup/',
  status,
  index_etag: '"etag-2"',
  index_version_id: 'vid-2',
  fetched_at: '2026-11-05T13:00:00+09:00',
  last_write_at: '',
  last_verified_at: '2026-11-05T13:00:00+09:00',
  entry_count,
})

const responseOf = (status: number, body: unknown): Response =>
  new Response(JSON.stringify(body), {
    status,
    headers: { 'Content-Type': 'application/json' },
  })

beforeEach(() => {
  setActivePinia(createPinia())
})

afterEach(() => {
  vi.unstubAllGlobals()
})

// [covers:frontend.health-state-store]
describe('sync storeのfetchHealth', () => {
  it('成功時はhealth=ok・message・lastError=nullへ遷移する', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(
        new Response(JSON.stringify({ status: 'ok' }), {
          status: 200,
          headers: { 'Content-Type': 'application/json' },
        }),
      ),
    )
    const sync = useSyncStore()
    await sync.fetchHealth()
    expect(sync.health).toBe('ok')
    expect(sync.message).toBe('ok')
    expect(sync.lastError).toBeNull()
  })

  it('失敗時はhealth=error・lastError(ApiError)を保持する', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockRejectedValue(new TypeError('fetch failed')),
    )
    const sync = useSyncStore()
    await sync.fetchHealth()
    expect(sync.health).toBe('error')
    expect(sync.message).toBe('')
    expect(sync.lastError).not.toBeNull()
    if (sync.lastError !== null) {
      expect(sync.lastError.code).toBe('network')
    }
  })
})

// [covers:frontend.sync-refresh-trigger]
describe('sync storeの手動refresh(refresh)', () => {
  it('refreshはPOST /api/syncでsync状態を更新した後entities一覧を再取得する', async () => {
    const calls: string[] = []
    let refreshingDuringPost = false
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((input: RequestInfo | URL, init?: RequestInit) => {
        const url = typeof input === 'string' ? input : input.toString()
        calls.push(`${init?.method ?? 'GET'} ${url}`)
        if (url === '/api/sync' && init?.method === 'POST') {
          refreshingDuringPost = useSyncStore().refreshing
          return Promise.resolve(responseOf(200, syncBodyOf('fresh', 2)))
        }
        if (url === '/api/entities') {
          return Promise.resolve(
            responseOf(200, { entities: [], page: 1, total_pages: 1 }),
          )
        }
        return Promise.reject(new TypeError(`unexpected fetch: ${url}`))
      }),
    )
    const sync = useSyncStore()
    expect(sync.refreshing).toBe(false)
    await sync.refresh()
    // 手動refresh(POST)の応答でsync状態(staleness表示)を更新し、その後で
    // entities一覧を再取得する(順序も含めて観測)
    expect(calls).toEqual(['POST /api/sync', 'GET /api/entities'])
    expect(refreshingDuringPost).toBe(true)
    expect(sync.refreshing).toBe(false)
    expect(sync.syncStatus).toBe('fresh')
    expect(sync.syncFetchedAt).toBe('2026-11-05T13:00:00+09:00')
    expect(sync.syncEntryCount).toBe(2)
    expect(sync.lastRefreshError).toBeNull()
  })

  it('POST失敗時はlastRefreshErrorを保持しentities再取得は行わない', async () => {
    const calls: string[] = []
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((input: RequestInfo | URL, init?: RequestInit) => {
        const url = typeof input === 'string' ? input : input.toString()
        calls.push(`${init?.method ?? 'GET'} ${url}`)
        return Promise.resolve(
          responseOf(502, {
            error: { code: 'upstream', message: 'remote索引の取得に失敗しました' },
          }),
        )
      }),
    )
    const sync = useSyncStore()
    await sync.refresh()
    expect(calls).toEqual(['POST /api/sync'])
    expect(sync.lastRefreshError?.code).toBe('upstream')
    expect(sync.refreshing).toBe(false)
  })
})

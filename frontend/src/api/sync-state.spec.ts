import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createPinia, setActivePinia } from 'pinia'
import { fetchSyncState, isSyncStateResponse, postSyncRefresh } from './client'
import { useSyncStore } from '../stores/sync'

// GET /api/sync応答の型ガードとsync状態取得(TASK-9設計6章・4章D10・12章P3)と
// 手動refresh POST /api/sync client(TASK-9設計4章D10・コードレビュー指摘高4)。
// fetchSyncState/isSyncStateResponseはP3実装済み・postSyncRefreshは実装前のため
// 該当testは実行failを許容(実装後に緑化)。

beforeEach(() => {
  setActivePinia(createPinia())
})

afterEach(() => {
  vi.unstubAllGlobals()
})

// [covers:frontend.sync-state-endpoint-client]
describe('sync状態の型ガード', () => {
  it('GET /api/sync応答形式を判定する', () => {
    const body = {
      scope: 'api-bucket:amazing-backup/',
      status: 'fresh',
      index_etag: '"etag-1"',
      index_version_id: 'vid-1',
      fetched_at: '2026-11-05T12:00:00+09:00',
      last_write_at: '',
      last_verified_at: '2026-11-05T12:00:00+09:00',
      entry_count: 1,
    }
    expect(isSyncStateResponse(body)).toBe(true)
    expect(isSyncStateResponse({ ...body, status: 'unknown' })).toBe(true)
    expect(isSyncStateResponse({ ...body, status: 1 })).toBe(false)
    expect(isSyncStateResponse({ status: 'fresh' })).toBe(false)
    expect(isSyncStateResponse(null)).toBe(false)
  })

  it('fetchSyncStateは200のsync状態を返す', async () => {
    const body = {
      scope: 'api-bucket:amazing-backup/',
      status: 'fresh',
      index_etag: '"etag-1"',
      index_version_id: 'vid-1',
      fetched_at: '2026-11-05T12:00:00+09:00',
      last_write_at: '',
      last_verified_at: '2026-11-05T12:00:00+09:00',
      entry_count: 1,
    }
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(
        new Response(JSON.stringify(body), {
          status: 200,
          headers: { 'Content-Type': 'application/json' },
        }),
      ),
    )
    const result = await fetchSyncState()
    expect(result.ok).toBe(true)
    if (result.ok) {
      expect(result.data.status).toBe('fresh')
      expect(result.data.scope).toBe('api-bucket:amazing-backup/')
      expect(result.data.entry_count).toBe(1)
    }
  })

  it('P2〜P3のfail-closed(503 configuration)はerror扱いになりdataを返さない', async () => {
    // mockImplementation: Responseのbodyは1回しか読めないため、client直呼び出しと
    // store経由の2回のfetchそれぞれに新しいResponseを渡す(mockResolvedValueの
    // 同一object再利用は2回目がbody消費済みで必ず失敗する)
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation(() =>
        Promise.resolve(
          new Response(
            JSON.stringify({
              error: {
                code: 'configuration',
                message: '保護APIは認証機構の構成が完了するまで閉鎖されています',
              },
            }),
            { status: 503, headers: { 'Content-Type': 'application/json' } },
          ),
        ),
      ),
    )
    const result = await fetchSyncState()
    expect(result.ok).toBe(false)
    if (!result.ok) {
      expect(result.error.code).toBe('configuration')
      expect(result.httpStatus).toBe(503)
    }
    // storeも閉鎖状態をlastErrorとして保持する(data表示をしない)
    const sync = useSyncStore()
    await sync.fetchSyncState()
    expect(sync.lastError).not.toBeNull()
    if (sync.lastError !== null) {
      expect(sync.lastError.code).toBe('configuration')
    }
  })
})

// [covers:frontend.sync-refresh-trigger]
describe('POST /api/sync client(手動refresh)', () => {
  it('postSyncRefreshはPOST /api/syncを呼び応答(sync状態形式)を検証してmappingする', async () => {
    const captured: string[] = []
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((input: RequestInfo | URL, init?: RequestInit) => {
        const url = typeof input === 'string' ? input : input.toString()
        captured.push(`${init?.method ?? 'GET'} ${url}`)
        return Promise.resolve(
          new Response(
            JSON.stringify({
              scope: 'api-bucket:amazing-backup/',
              status: 'fresh',
              index_etag: '"etag-2"',
              index_version_id: 'vid-2',
              fetched_at: '2026-11-05T13:00:00+09:00',
              last_write_at: '',
              last_verified_at: '2026-11-05T13:00:00+09:00',
              entry_count: 2,
            }),
            { status: 200, headers: { 'Content-Type': 'application/json' } },
          ),
        )
      }),
    )
    const result = await postSyncRefresh()
    expect(result.ok).toBe(true)
    if (result.ok) {
      expect(result.data.status).toBe('fresh')
      expect(result.data.entry_count).toBe(2)
    }
    expect(captured).toEqual(['POST /api/sync'])
  })

  it('POST失敗(502 upstream)はerror扱いにする(保護API)', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation(() =>
        Promise.resolve(
          new Response(
            JSON.stringify({
              error: { code: 'upstream', message: 'remote索引の取得に失敗しました' },
            }),
            { status: 502, headers: { 'Content-Type': 'application/json' } },
          ),
        ),
      ),
    )
    const result = await postSyncRefresh()
    expect(result.ok).toBe(false)
    if (!result.ok) {
      expect(result.error.code).toBe('upstream')
      expect(result.httpStatus).toBe(502)
    }
  })
})

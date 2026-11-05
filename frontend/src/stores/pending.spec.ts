import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createPinia, setActivePinia } from 'pinia'
import { usePendingStore } from './pending'

// pending storeの一覧保持・status別集計・scan実行(TASK-9設計7章・12章P5)。
// P5実装前のためsrc/stores/pending.tsは未実装=本fileのtestは実行failを許容
// (実装後に緑化)。PendingViewの実描画は実browser観測のため条件書frontend.tomlの
// excluded参照(一覧のUI状態はcounts getter・scanはrunScan actionで担保)。

const itemOf = (path: string, status: string) => ({
  source_root: '/data',
  path,
  size: 1024,
  mtime: '2026-11-05T10:00:00.000000+09:00',
  status,
  last_scan_at: '2026-11-05T11:00:00.000000+09:00',
})

beforeEach(() => {
  setActivePinia(createPinia())
})

afterEach(() => {
  vi.unstubAllGlobals()
})

// [covers:frontend.pending-client-and-store]
describe('pending storeの一覧取得(fetchPending)', () => {
  it('成功時はitemsとstatus別集計(counts)を保持する', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((input: RequestInfo | URL) => {
        const url = typeof input === 'string' ? input : input.toString()
        expect(url).toBe('/api/pending')
        return Promise.resolve(
          new Response(
            JSON.stringify({
              pending: [
                itemOf('/data/a.zip', 'unbacked'),
                itemOf('/data/b.zip', 'changed'),
                itemOf('/data/c.zip', 'unbacked'),
              ],
            }),
            { status: 200, headers: { 'Content-Type': 'application/json' } },
          ),
        )
      }),
    )
    const pending = usePendingStore()
    await pending.fetchPending()
    expect(pending.items.length).toBe(3)
    expect(pending.counts.unbacked).toBe(2)
    expect(pending.counts.changed).toBe(1)
    expect(pending.counts.ok).toBe(0)
    expect(pending.counts.unknown).toBe(0)
    expect(pending.lastError).toBeNull()
  })

  it('失敗時はlastErrorを保持し一覧を空へ戻す(空dataを表示しない)', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockRejectedValue(new TypeError('fetch failed')),
    )
    const pending = usePendingStore()
    await pending.fetchPending()
    expect(pending.items.length).toBe(0)
    expect(pending.lastError).not.toBeNull()
  })
})

// [covers:frontend.pending-scan-trigger]
describe('pending storeのscan実行(runScan)', () => {
  it('scanはPOST /api/pending/scan実行後に一覧を再取得する', async () => {
    const calls: string[] = []
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((input: RequestInfo | URL, init?: RequestInit) => {
        const url = typeof input === 'string' ? input : input.toString()
        calls.push(`${init?.method ?? 'GET'} ${url}`)
        if (url === '/api/pending/scan') {
          return Promise.resolve(
            new Response(JSON.stringify({ pending: [] }), {
              status: 200,
              headers: { 'Content-Type': 'application/json' },
            }),
          )
        }
        return Promise.resolve(
          new Response(
            JSON.stringify({ pending: [itemOf('/data/a.zip', 'unbacked')] }),
            { status: 200, headers: { 'Content-Type': 'application/json' } },
          ),
        )
      }),
    )
    const pending = usePendingStore()
    expect(pending.scanning).toBe(false)
    await pending.runScan()
    expect(calls[0]).toBe('POST /api/pending/scan')
    expect(calls[1]).toBe('GET /api/pending')
    expect(pending.items.length).toBe(1)
    expect(pending.scanning).toBe(false)
  })
})

// [covers:frontend.pending-rerun-error-visible]
describe('pending storeの再実行失敗の明示(runScan失敗と取得成功の分離)', () => {
  it('scan(POST)失敗後の一覧再取得が成功しても直前のscan失敗は隠されない', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((input: RequestInfo | URL, init?: RequestInit) => {
        const url = typeof input === 'string' ? input : input.toString()
        if (url === '/api/pending/scan' && init?.method === 'POST') {
          return Promise.resolve(
            new Response(
              JSON.stringify({ error: { code: 'internal', message: '内部エラーが発生しました' } }),
              { status: 500, headers: { 'Content-Type': 'application/json' } },
            ),
          )
        }
        return Promise.resolve(
          new Response(
            JSON.stringify({ pending: [itemOf('/data/a.zip', 'unbacked')] }),
            { status: 200, headers: { 'Content-Type': 'application/json' } },
          ),
        )
      }),
    )
    const pending = usePendingStore()
    await pending.runScan()
    // 一覧再取得は成功している(旧データを表示できる)
    expect(pending.items.length).toBe(1)
    expect(pending.lastError).toBeNull()
    // 直前のscan実行の失敗は別状態で保持され表示から隠されない
    expect(pending.lastScanError?.code).toBe('internal')
  })

  it('scan成功でlastScanErrorはクリアされる', async () => {
    const pending = usePendingStore()
    pending.lastScanError = { code: 'internal', message: '内部エラーが発生しました' }
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((input: RequestInfo | URL, init?: RequestInit) => {
        const url = typeof input === 'string' ? input : input.toString()
        if (url === '/api/pending/scan' && init?.method === 'POST') {
          return Promise.resolve(
            new Response(JSON.stringify({ pending: [] }), {
              status: 200,
              headers: { 'Content-Type': 'application/json' },
            }),
          )
        }
        return Promise.resolve(
          new Response(JSON.stringify({ pending: [] }), {
            status: 200,
            headers: { 'Content-Type': 'application/json' },
          }),
        )
      }),
    )
    await pending.runScan()
    expect(pending.lastScanError).toBeNull()
  })
})

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createPinia, setActivePinia } from 'pinia'
import { useRunsStore } from './runs'

// runs store(TASK-17設計2.1・9章W1): ホームbackup概况=前回backup(直近runの日時・
// 対象数)の表示基盤。latestRun getterは降順応答の先頭要素を直近runとして返す
// (server.runs-listの並び解釈に整合)。実装前のためsrc/stores/runs.tsは未実装=
// 本fileのtestは実行failを許容(実装後に緑化)。

const runsBody = {
  runs: [
    { batch_id: '20261106-abcdef02', date: '2026-11-06', entity_count: 2 },
    { batch_id: '20261105-abcdef01', date: '2026-11-05', entity_count: 1 },
  ],
}

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

// [covers:frontend.runs-client-and-store]
describe('runs store(fetchRuns・latestRun)', () => {
  it('成功時はrun一覧を保持しlatestRun getterは直近run(降順先頭)を返す', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation(() => Promise.resolve(responseOf(200, runsBody))),
    )
    const runs = useRunsStore()
    await runs.fetchRuns()
    expect(runs.items).toHaveLength(2)
    expect(runs.lastError).toBeNull()
    expect(runs.latestRun?.batch_id).toBe('20261106-abcdef02')
    expect(runs.latestRun?.date).toBe('2026-11-06')
    expect(runs.latestRun?.entity_count).toBe(2)
  })

  it('失敗時はlastErrorを保持し保持済みitemsは空へ戻りlatestRunはnull(空dataを表示しない)', async () => {
    // 非空一覧を保持した状態での再取得失敗=保持済みitemsの消去も観測する
    // (空の初期stateからの失敗だけではitemsを空へ戻す実装を被覆できない)
    vi.stubGlobal(
      'fetch',
      vi
        .fn()
        .mockImplementationOnce(() => Promise.resolve(responseOf(200, runsBody)))
        .mockImplementationOnce(() =>
          Promise.resolve(
            responseOf(500, { error: { code: 'internal', message: '内部エラーです' } }),
          ),
        ),
    )
    const runs = useRunsStore()
    await runs.fetchRuns()
    expect(runs.items).toHaveLength(2)
    expect(runs.latestRun?.batch_id).toBe('20261106-abcdef02')
    await runs.fetchRuns()
    expect(runs.items).toEqual([])
    expect(runs.lastError?.code).toBe('internal')
    expect(runs.latestRun).toBeNull()
  })
})

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createPinia, setActivePinia } from 'pinia'
import { useDiffStore } from './diff'

// diff storeの結果保持・diff実行・DiffView UI状態(TASK-9設計7章・12章P6=diff UI)。
// P6実装前のためsrc/stores/diff.tsは未実装=本fileのtestは実行failを許容
// (実装後に緑化)。DiffViewの実描画は実browser観測のため条件書frontend.tomlの
// excluded参照(UI誘導状態はdiffView getter・結果集計はsummary getterで担保)。

const diffBody = {
  performed_at: '2026-11-05T12:00:00+09:00',
  entries: 2,
  files: [
    { uuid: '0192a4f0-0000-7000-8000-000000000001', title: 'a.zip' },
    { uuid: '0192a4f0-0000-7000-8000-000000000002', title: 'b.zip' },
  ],
  missing: [
    'data/0192a4f0-0000-7000-8000-000000000001/default.age',
    'data/0192a4f0-0000-7000-8000-000000000002/default.age',
  ],
  unexpected: ['data/unknown/default.age'],
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

// [covers:frontend.diff-client-and-store]
describe('diff storeの結果取得(fetchDiff)', () => {
  it('成功時はdiff結果(performed_at等)を保持する', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((input: RequestInfo | URL, init?: RequestInit) => {
        const url = typeof input === 'string' ? input : input.toString()
        expect(url).toBe('/api/diff')
        expect(init?.method ?? 'GET').toBe('GET')
        return Promise.resolve(responseOf(200, diffBody))
      }),
    )
    const diff = useDiffStore()
    await diff.fetchDiff()
    expect(diff.result?.performed_at).toBe('2026-11-05T12:00:00+09:00')
    expect(diff.result?.entries).toBe(2)
    expect(diff.lastError).toBeNull()
  })

  it('未実行404 not_foundはlastErrorを保持しresultはnull(空dataを表示しない)', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation(() =>
        Promise.resolve(
          responseOf(404, { error: { code: 'not_found', message: 'diff結果は未実行です' } }),
        ),
      ),
    )
    const diff = useDiffStore()
    await diff.fetchDiff()
    expect(diff.result).toBeNull()
    expect(diff.lastError?.code).toBe('not_found')
  })

  it('network失敗はlastErrorを保持しresultはnull', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockRejectedValue(new TypeError('fetch failed')),
    )
    const diff = useDiffStore()
    await diff.fetchDiff()
    expect(diff.result).toBeNull()
    expect(diff.lastError?.code).toBe('network')
  })
})

// [covers:frontend.diff-run-trigger]
describe('diff storeのdiff実行(runDiff)', () => {
  it('runDiffはPOST /api/diffの1回のみ呼び実行中状態を遷移させPOST応答を保持する', async () => {
    const calls: string[] = []
    let runningDuringPost = false
    let viewDuringPost = ''
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((input: RequestInfo | URL, init?: RequestInit) => {
        const url = typeof input === 'string' ? input : input.toString()
        calls.push(`${init?.method ?? 'GET'} ${url}`)
        if (url === '/api/diff' && init?.method === 'POST') {
          const diff = useDiffStore()
          runningDuringPost = diff.running
          viewDuringPost = diff.diffView
        }
        return Promise.resolve(responseOf(200, diffBody))
      }),
    )
    const diff = useDiffStore()
    expect(diff.running).toBe(false)
    await diff.runDiff()
    // POST応答がdiff結果そのもののため実行後の再GETは行わない
    expect(calls).toEqual(['POST /api/diff'])
    expect(runningDuringPost).toBe(true)
    expect(viewDuringPost).toBe('running')
    expect(diff.running).toBe(false)
    expect(diff.result?.performed_at).toBe('2026-11-05T12:00:00+09:00')
    expect(diff.lastError).toBeNull()
  })
})

// [covers:frontend.diff-view-state]
describe('diff storeのdiffView getter(DiffView UI誘導状態)', () => {
  it('未取得・未実行404はnot_run・その他の失敗はerror・結果保持済みはresult', async () => {
    const diff = useDiffStore()
    expect(diff.diffView).toBe('not_run')

    // 未実行diffの404 not_foundは未実行(not_run)扱い
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation(() =>
        Promise.resolve(
          responseOf(404, { error: { code: 'not_found', message: 'diff結果は未実行です' } }),
        ),
      ),
    )
    await diff.fetchDiff()
    expect(diff.diffView).toBe('not_run')

    // 未認証401はerror扱い(not_foundと区別)
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation(() =>
        Promise.resolve(
          responseOf(401, {
            error: { code: 'unauthenticated', message: '認証が必要です' },
          }),
        ),
      ),
    )
    await diff.fetchDiff()
    expect(diff.diffView).toBe('error')

    // 結果取得済みはresult
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation(() => Promise.resolve(responseOf(200, diffBody))),
    )
    await diff.fetchDiff()
    expect(diff.diffView).toBe('result')
  })

  it('summary getterはentries・missing・unexpectedの件数を集計する', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation(() => Promise.resolve(responseOf(200, diffBody))),
    )
    const diff = useDiffStore()
    expect(diff.summary).toEqual({ entries: 0, missing: 0, unexpected: 0 })
    await diff.fetchDiff()
    expect(diff.summary).toEqual({ entries: 2, missing: 2, unexpected: 1 })
  })
})

// [covers:frontend.diff-rerun-error-visible]
describe('diff storeの再実行失敗の明示(runDiff失敗で旧result保持)', () => {
  it('再実行失敗時は旧resultを保持しlastRunErrorを別状態で示す(旧結果のみ表示しない)', async () => {
    // まず取得に成功してresultを保持させる
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation(() => Promise.resolve(responseOf(200, diffBody))),
    )
    const diff = useDiffStore()
    await diff.fetchDiff()
    expect(diff.result).not.toBeNull()

    // 再実行(POST)が失敗: 旧resultは保持され実行errorは別状態へ
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation(() =>
        Promise.resolve(
          responseOf(500, {
            error: { code: 'internal', message: '内部エラーが発生しました' },
          }),
        ),
      ),
    )
    await diff.runDiff()
    // 実行失敗で旧結果を破棄しない
    expect(diff.result).not.toBeNull()
    expect(diff.lastRunError?.code).toBe('internal')
    // 取得系errorとは別状態
    expect(diff.lastError).toBeNull()
    expect(diff.diffView).toBe('result')
    // 旧結果を表示している状態での実行失敗の明示フラグ(Viewの案内行用)
    expect(diff.showingStaleResult).toBe(true)

    // 再実行が成功すれば実行errorはクリアされ明示も消える
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation(() => Promise.resolve(responseOf(200, diffBody))),
    )
    await diff.runDiff()
    expect(diff.lastRunError).toBeNull()
    expect(diff.showingStaleResult).toBe(false)
  })
})

// [covers:frontend.diff-run-error-visible-without-result]
describe('diff storeの初回実行失敗の明示(保存済み結果なし)', () => {
  it('保存済みresultが無い状態での実行失敗もlastRunErrorで示し表示対象になる', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation(() =>
        Promise.resolve(
          responseOf(500, {
            error: { code: 'internal', message: '内部エラーが発生しました' },
          }),
        ),
      ),
    )
    const diff = useDiffStore()
    expect(diff.result).toBeNull()
    await diff.runDiff()
    // 旧resultが無い場合でも実行errorは保持され表示条件(result有無と無関係)を満たす
    expect(diff.result).toBeNull()
    expect(diff.lastRunError?.code).toBe('internal')
    expect(diff.hasRunError).toBe(true)
    expect(diff.showingStaleResult).toBe(false)

    // 実行成功で実行errorはクリアされる
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation(() => Promise.resolve(responseOf(200, diffBody))),
    )
    await diff.runDiff()
    expect(diff.hasRunError).toBe(false)
    expect(diff.lastRunError).toBeNull()
  })
})

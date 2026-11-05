import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createPinia, setActivePinia } from 'pinia'
import { useEntitiesStore } from './entities'

// entities storeの一覧・検索・絞り込み・paging・詳細(TASK-9設計7章=
// EntitiesView(一覧・検索・絞込)・EntityDetailView・コードレビュー指摘高4)。
// 実装前のためsrc/stores/entities.tsは未実装=本fileのtestは実行failを許容
// (実装後に緑化)。実描画は実browser観測のため条件書frontend.tomlのexcluded参照
// (UI誘導状態はentitiesView getter・絞込はquery/category状態で担保)。

const summaryOf = (uuid: string, title: string) => ({
  uuid,
  title,
  locations: [
    {
      source_path: `/data/${title}`,
      category: 'docs',
      label: 'work',
      identity_hash: 'sha256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=',
      last_backup: '2026-11-05T12:00:00+09:00',
    },
  ],
  versions: 1,
  latest_date: '2026-11-05',
  latest_size: 1024,
  pins: [],
})

const UUID_A = '0192a4f0-0000-7000-8000-000000000001'
const UUID_B = '0192a4f0-0000-7000-8000-000000000002'

const listBody = (uuids: string[], page: number, total_pages: number) => ({
  entities: uuids.map((u, i) => summaryOf(u, `file-${i}.zip`)),
  page,
  total_pages,
})

const detailBody = {
  ...summaryOf(UUID_A, 'a.zip'),
  version_list: [
    {
      date: '2026-11-05',
      version_ts: '2026-11-05T12:00:00Z',
      version_id: 'vid-2',
      batch_id: 'batch-2',
      source_size: 1024,
    },
  ],
}

const versionsBody = {
  uuid: UUID_A,
  title: 'a.zip',
  locations: summaryOf(UUID_A, 'a.zip').locations,
  versions: [
    {
      date: '2026-11-05',
      version_ts: '2026-11-05T12:00:00Z',
      version_id: 'vid-2',
      source_size: 1024,
      state: '現行',
    },
  ],
  pins: [],
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

// [covers:frontend.entities-store]
describe('entities storeの一覧取得(fetchEntities)', () => {
  it('成功時はitems・page・totalPagesを保持しloadingはtrueからfalseへ戻る', async () => {
    let loadingDuringFetch = false
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation(() => {
        loadingDuringFetch = useEntitiesStore().loading
        return Promise.resolve(responseOf(200, listBody([UUID_A, UUID_B], 1, 3)))
      }),
    )
    const entities = useEntitiesStore()
    expect(entities.loading).toBe(false)
    await entities.fetchEntities()
    expect(loadingDuringFetch).toBe(true)
    expect(entities.items.length).toBe(2)
    expect(entities.items[0].uuid).toBe(UUID_A)
    expect(entities.page).toBe(1)
    expect(entities.totalPages).toBe(3)
    expect(entities.lastError).toBeNull()
    expect(entities.loading).toBe(false)
  })

  it('失敗時はlastErrorを保持しitemsを空へ戻す(空dataを表示しない)', async () => {
    // 先に1件取得済みの状態を作り、失敗時に一覧が空へ戻ることを観測する
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation(() =>
        Promise.resolve(responseOf(200, listBody([UUID_A], 1, 1))),
      ),
    )
    const entities = useEntitiesStore()
    await entities.fetchEntities()
    expect(entities.items.length).toBe(1)

    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation(() =>
        Promise.resolve(
          responseOf(401, { error: { code: 'unauthenticated', message: '認証が必要です' } }),
        ),
      ),
    )
    await entities.fetchEntities()
    expect(entities.items.length).toBe(0)
    expect(entities.lastError?.code).toBe('unauthenticated')
  })

  it('setQuery/setCategoryはpageを1へ戻し再取得のURL queryへ反映される', async () => {
    const captured: string[] = []
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((input: RequestInfo | URL) => {
        captured.push(typeof input === 'string' ? input : input.toString())
        return Promise.resolve(responseOf(200, listBody([], 1, 1)))
      }),
    )
    const entities = useEntitiesStore()
    // 2page目を閲覧中の状態で検索語を変更すると先頭pageへ戻る
    entities.page = 2
    entities.setQuery('beta')
    expect(entities.query).toBe('beta')
    expect(entities.page).toBe(1)
    await entities.fetchEntities()
    expect(captured[0]).toContain('query=beta')
    expect(captured[0]).toContain('page=1')

    // 絞り込み(category)追加もpage=1を維持し検索語と併せてURLへ乗る
    entities.setCategory('docs')
    expect(entities.category).toBe('docs')
    expect(entities.page).toBe(1)
    await entities.fetchEntities()
    expect(captured[1]).toContain('query=beta')
    expect(captured[1]).toContain('category=docs')
  })

  it('setPageは指定pageで取得する', async () => {
    const captured: string[] = []
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((input: RequestInfo | URL) => {
        captured.push(typeof input === 'string' ? input : input.toString())
        return Promise.resolve(responseOf(200, listBody([], 2, 3)))
      }),
    )
    const entities = useEntitiesStore()
    entities.setPage(2)
    expect(entities.page).toBe(2)
    await entities.fetchEntities()
    expect(captured[0]).toBe('/api/entities?page=2')
    expect(entities.page).toBe(2)
  })
})

// [covers:frontend.entities-view-state]
describe('entities storeのUI状態(entitiesView getter)', () => {
  it('未取得=not_fetched・成功=result・失敗=error・取得中=loading', async () => {
    const entities = useEntitiesStore()
    expect(entities.entitiesView).toBe('not_fetched')

    // 成功(0件の成功も有効なdataのためresult)
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation(() => Promise.resolve(responseOf(200, listBody([], 1, 1)))),
    )
    await entities.fetchEntities()
    expect(entities.entitiesView).toBe('result')

    // 失敗はerror
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation(() =>
        Promise.resolve(
          responseOf(401, { error: { code: 'unauthenticated', message: '認証が必要です' } }),
        ),
      ),
    )
    await entities.fetchEntities()
    expect(entities.entitiesView).toBe('error')

    // 取得中はloading(最優先)
    let viewDuringFetch = ''
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation(() => {
        viewDuringFetch = useEntitiesStore().entitiesView
        return Promise.resolve(responseOf(200, listBody([UUID_A], 1, 1)))
      }),
    )
    await entities.fetchEntities()
    expect(viewDuringFetch).toBe('loading')
    expect(entities.entitiesView).toBe('result')
  })
})

// [covers:frontend.entities-view-state]
describe('entities storeの詳細・versions取得(EntityDetailView基盤)', () => {
  it('fetchEntityDetailはdetail(version_list)とselectedUuidを保持する', async () => {
    const captured: string[] = []
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((input: RequestInfo | URL) => {
        captured.push(typeof input === 'string' ? input : input.toString())
        return Promise.resolve(responseOf(200, detailBody))
      }),
    )
    const entities = useEntitiesStore()
    await entities.fetchEntityDetail(UUID_A)
    expect(captured[0]).toBe(`/api/entities/${UUID_A}`)
    expect(entities.selectedUuid).toBe(UUID_A)
    expect(entities.detail?.version_list.length).toBe(1)
    expect(entities.detail?.version_list[0].version_id).toBe('vid-2')
    expect(entities.detailError).toBeNull()
  })

  it('不在uuidの404 not_foundはdetailErrorで示しdetailは保持しない', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation(() =>
        Promise.resolve(
          responseOf(404, {
            error: { code: 'not_found', message: '対象のentityが見つかりません' },
          }),
        ),
      ),
    )
    const entities = useEntitiesStore()
    await entities.fetchEntityDetail('99999999-9999-9999-9999-999999999999')
    expect(entities.detail).toBeNull()
    expect(entities.detailError?.code).toBe('not_found')
  })

  it('fetchEntityVersionsはstate付きの版一覧を保持する', async () => {
    const captured: string[] = []
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((input: RequestInfo | URL) => {
        captured.push(typeof input === 'string' ? input : input.toString())
        return Promise.resolve(responseOf(200, versionsBody))
      }),
    )
    const entities = useEntitiesStore()
    await entities.fetchEntityVersions(UUID_A)
    expect(captured[0]).toBe(`/api/entities/${UUID_A}/versions`)
    expect(entities.versions?.versions.length).toBe(1)
    expect(entities.versions?.versions[0].state).toBe('現行')
  })
})

// [covers:frontend.entities-filter-apply]
describe('entities storeの絞り込み適用(applyFilters)', () => {
  it('検索語とcategoryを1回の適用でURL queryへ同時反映する(1回のfetchのみ)', async () => {
    const captured: string[] = []
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((input: RequestInfo | URL) => {
        captured.push(typeof input === 'string' ? input : input.toString())
        return Promise.resolve(responseOf(200, listBody([UUID_A], 1, 1)))
      }),
    )
    const entities = useEntitiesStore()
    // 2page閲覧中の状態から両入力を1回のsubmitで適用する(先頭pageへ戻る)
    entities.page = 2
    await entities.applyFilters('beta', 'docs')
    expect(captured).toHaveLength(1)
    expect(captured[0]).toBe('/api/entities?query=beta&category=docs&page=1')
    expect(entities.query).toBe('beta')
    expect(entities.category).toBe('docs')
    expect(entities.page).toBe(1)

    // category無し(null)での適用はcategory paramを付けない
    await entities.applyFilters('', null)
    expect(captured).toHaveLength(2)
    expect(captured[1]).toBe('/api/entities?page=1')
  })
})

// [covers:frontend.sync-refresh-invalidates-detail-state]
describe('entities storeのsync後再取得(refetchEntities)の詳細失効', () => {
  it('refetchEntitiesは詳細・versionsの保持内容を失効させる(旧詳細と新一覧の混在防止)', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((input: RequestInfo | URL) => {
        const url = typeof input === 'string' ? input : input.toString()
        if (url === `/api/entities/${UUID_A}`) {
          return Promise.resolve(responseOf(200, detailBody))
        }
        if (url === `/api/entities/${UUID_A}/versions`) {
          return Promise.resolve(responseOf(200, versionsBody))
        }
        return Promise.resolve(responseOf(200, listBody([UUID_A], 1, 1)))
      }),
    )
    const entities = useEntitiesStore()
    await entities.fetchEntityDetail(UUID_A)
    await entities.fetchEntityVersions(UUID_A)
    expect(entities.selectedUuid).toBe(UUID_A)
    expect(entities.detail).not.toBeNull()
    expect(entities.versions).not.toBeNull()

    // 手動sync後の再取得: 一覧は更新し詳細系状態は失効する(syncで索引が置き換わる
    // ため詳細を開く際に再取得させる)
    await entities.refetchEntities()
    expect(entities.items.length).toBe(1)
    expect(entities.selectedUuid).toBeNull()
    expect(entities.detail).toBeNull()
    expect(entities.detailError).toBeNull()
    expect(entities.versions).toBeNull()
    expect(entities.versionsError).toBeNull()
  })
})

// [covers:frontend.entities-detail-versions-error-separation]
describe('entities storeの詳細・versions error分離', () => {
  it('詳細404失敗後にversions取得が成功してもdetailErrorは残る(成功で失敗を隠さない)', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((input: RequestInfo | URL) => {
        const url = typeof input === 'string' ? input : input.toString()
        if (url === `/api/entities/${UUID_A}`) {
          return Promise.resolve(
            responseOf(404, {
              error: { code: 'not_found', message: '対象のentityが見つかりません' },
            }),
          )
        }
        return Promise.resolve(responseOf(200, versionsBody))
      }),
    )
    const entities = useEntitiesStore()
    await entities.fetchEntityDetail(UUID_A)
    await entities.fetchEntityVersions(UUID_A)
    expect(entities.detail).toBeNull()
    expect(entities.detailError?.code).toBe('not_found')
    expect(entities.versions).not.toBeNull()
    expect(entities.versionsError).toBeNull()
  })

  it('versions失敗後に詳細取得が成功してもversionsErrorは残る', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((input: RequestInfo | URL) => {
        const url = typeof input === 'string' ? input : input.toString()
        if (url === `/api/entities/${UUID_A}/versions`) {
          return Promise.resolve(
            responseOf(500, {
              error: { code: 'internal', message: '内部エラーが発生しました' },
            }),
          )
        }
        return Promise.resolve(responseOf(200, detailBody))
      }),
    )
    const entities = useEntitiesStore()
    await entities.fetchEntityVersions(UUID_A)
    await entities.fetchEntityDetail(UUID_A)
    expect(entities.detail).not.toBeNull()
    expect(entities.detailError).toBeNull()
    expect(entities.versions).toBeNull()
    expect(entities.versionsError?.code).toBe('internal')
  })
})

// [covers:frontend.entities-stale-detail-response-rejected]
describe('entities storeの失効をまたいだ詳細応答(detailEpoch)', () => {
  it('失効(sync後再取得)前に開始した応答は完了しても採用されない(旧内容の再格納防止)', async () => {
    // 詳細応答のみ保留(gate)とし、失効をまたいでから完了させる。callごとに
    // 新しいResponseを返す(Response本体は再読取不可のため共用しない)。
    // Promise executorは同期実行のためreleaseDetailは即座にresolveへ置換される
    let releaseDetail: () => void = () => {}
    const detailGate = new Promise<void>((resolve) => {
      releaseDetail = resolve
    })
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((input: RequestInfo | URL) => {
        const url = typeof input === 'string' ? input : input.toString()
        if (url === `/api/entities/${UUID_A}`) {
          return detailGate.then(() => responseOf(200, detailBody))
        }
        if (url === `/api/entities/${UUID_A}/versions`) {
          return Promise.resolve(responseOf(200, versionsBody))
        }
        return Promise.resolve(responseOf(200, listBody([UUID_A], 1, 1)))
      }),
    )
    const entities = useEntitiesStore()
    const detailPromise = entities.fetchEntityDetail(UUID_A)
    const versionsPromise = entities.fetchEntityVersions(UUID_A)

    // 応答が完了する前に手動sync後の一覧再取得が走り詳細・versionsは失効する
    await entities.refetchEntities()
    expect(entities.detail).toBeNull()
    expect(entities.versions).toBeNull()

    // 失効前に開始した旧応答がここで完了しても旧内容は再格納されない
    releaseDetail()
    await detailPromise
    await versionsPromise
    expect(entities.detail).toBeNull()
    expect(entities.detailError).toBeNull()
    expect(entities.versions).toBeNull()
    expect(entities.versionsError).toBeNull()

    // 失効後の新規取得は採用される(表示中詳細の再取得経路)
    await entities.fetchEntityDetail(UUID_A)
    await entities.fetchEntityVersions(UUID_A)
    expect(entities.detail).not.toBeNull()
    expect(entities.versions).not.toBeNull()
    expect(entities.selectedUuid).toBe(UUID_A)
  })
})

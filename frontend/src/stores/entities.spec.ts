import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createPinia, setActivePinia } from 'pinia'
import { useEntitiesStore } from './entities'
import { useTagsStore } from './tags'

// entities storeの一覧・検索・絞り込み・paging・詳細(TASK-9設計7章=
// EntitiesView(一覧・検索・絞込)・EntityDetailView・コードレビュー指摘高4)。
// 実描画は実browser観測のため条件書frontend.tomlのexcluded参照
// (UI誘導状態はentitiesView getter・絞込はquery/category状態で担保)。
// W2(TASK-17設計2.2・9章W2=TASK-19)分の追記: ビュー切替状態・tag/collection/label
// 絞り込み・tag編集(updateTags)。実装前のため追記分のtestは実行failを許容
// (実装後に緑化)。

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
  tags: [],
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
  tags: ['work'],
  collections: [],
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

// --- W2(TASK-17設計2.2・9章W2=TASK-19): 3ビュー切替・tag/collection絞り込み・tag編集 ---
// 実装前のため本区画のtestは実行failを許容(実装後に緑化)。

// [covers:frontend.entities-tag-collection-filter]
describe('entities storeのtag/collection絞り込み(W2)', () => {
  it('setTagFilterはpageを1へ戻しURL queryのtag=へ反映する(検索語・pagerと共存)', async () => {
    const captured: string[] = []
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((input: RequestInfo | URL) => {
        captured.push(typeof input === 'string' ? input : input.toString())
        return Promise.resolve(responseOf(200, listBody([], 1, 1)))
      }),
    )
    const entities = useEntitiesStore()
    entities.setQuery('beta')
    entities.setPage(2)
    entities.setTagFilter('work')
    expect(entities.tagFilter).toBe('work')
    expect(entities.page).toBe(1)
    await entities.fetchEntities()
    expect(captured[0]).toContain('query=beta')
    expect(captured[0]).toContain('tag=work')
    expect(captured[0]).toContain('page=1')
    // pager(page指定)はtag絞り込みと共存して動く(検索box・pagerの3ビュー共通動作)
    entities.setPage(2)
    await entities.fetchEntities()
    expect(captured[1]).toContain('tag=work')
    expect(captured[1]).toContain('page=2')
    // 解除(null)はtag paramを付けない
    entities.setTagFilter(null)
    await entities.fetchEntities()
    expect(captured[2]).not.toContain('tag=')
  })

  it('setCollectionFilterはpageを1へ戻しcollection idをURL queryのcollection=へ反映する', async () => {
    const captured: string[] = []
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((input: RequestInfo | URL) => {
        captured.push(typeof input === 'string' ? input : input.toString())
        return Promise.resolve(responseOf(200, listBody([], 1, 1)))
      }),
    )
    const entities = useEntitiesStore()
    entities.setPage(2)
    entities.setCollectionFilter(3)
    expect(entities.collectionFilter).toBe(3)
    expect(entities.page).toBe(1)
    await entities.fetchEntities()
    expect(captured[0]).toContain('collection=3')
    expect(captured[0]).toContain('page=1')
  })
})

// [covers:frontend.entities-view-switch-state]
describe('entities storeのビュー切替状態(W2)', () => {
  it('初期viewはflat・setViewは選択系絞り込みをクリアしpageを1へ戻す(検索語・categoryは維持)', async () => {
    const captured: string[] = []
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((input: RequestInfo | URL) => {
        captured.push(typeof input === 'string' ? input : input.toString())
        return Promise.resolve(responseOf(200, listBody([], 1, 1)))
      }),
    )
    const entities = useEntitiesStore()
    expect(entities.view).toBe('flat')
    entities.setQuery('beta')
    entities.setCategory('docs')
    entities.setTagFilter('work')
    entities.setLabelFilter('docs/2026')
    entities.setPage(3)
    entities.setView('tag')
    expect(entities.view).toBe('tag')
    // 選択系絞り込み(tag/collection/label)はビュー切替でクリアしpageを1へ戻す
    expect(entities.tagFilter).toBeNull()
    expect(entities.collectionFilter).toBeNull()
    expect(entities.labelFilter).toBeNull()
    expect(entities.page).toBe(1)
    // 検索box(query)・category絞り込みは3ビュー共通で動くため維持される
    expect(entities.query).toBe('beta')
    expect(entities.category).toBe('docs')
    await entities.fetchEntities()
    expect(captured[0]).toContain('query=beta')
    expect(captured[0]).toContain('category=docs')
    expect(captured[0]).toContain('page=1')
    expect(captured[0]).not.toContain('tag=')
    expect(captured[0]).not.toContain('label=')
  })

  it('tag/collection/folder各ビューの選択はtag=/collection=/label=(既存)絞り込みへ接続する', async () => {
    const captured: string[] = []
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((input: RequestInfo | URL) => {
        captured.push(typeof input === 'string' ? input : input.toString())
        return Promise.resolve(responseOf(200, listBody([], 1, 1)))
      }),
    )
    const entities = useEntitiesStore()
    // tagビュー: 選択tagはtag=絞り込み(tag付きentity一覧)
    entities.setView('tag')
    entities.setTagFilter('work')
    await entities.fetchEntities()
    expect(captured[0]).toContain('tag=work')
    expect(captured[0]).toContain('page=1')
    // collectionビュー: 選択collectionはcollection=絞り込み(切替でtagFilterはクリア)
    entities.setView('collection')
    entities.setCollectionFilter(3)
    await entities.fetchEntities()
    expect(captured[1]).toContain('collection=3')
    expect(captured[1]).not.toContain('tag=')
    // folderビュー: 選択folderは既存label=絞り込み(folder絞り込みの新paramは設けない)
    entities.setView('folder')
    entities.setLabelFilter('docs/2026')
    await entities.fetchEntities()
    expect(captured[2]).toContain('label=docs%2F2026')
    expect(captured[2]).not.toContain('collection=')
    expect(captured[2]).toContain('page=1')
  })
})

// [covers:frontend.entity-tags-edit-client-and-store]
describe('entities storeのtag編集(updateTags・W2)', () => {
  const detailAfterPut = { ...detailBody, tags: ['work', 'nas'] }

  it('updateTagsはPUT /api/entities/:uuid/tagsを呼び応答(操作後の対象状態)でdetailを更新する(再GETしない)', async () => {
    const calls: string[] = []
    const bodies: unknown[] = []
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((input: RequestInfo | URL, init?: RequestInit) => {
        const url = typeof input === 'string' ? input : input.toString()
        calls.push(`${init?.method ?? 'GET'} ${url}`)
        const parsed: unknown =
          init?.body === undefined || init?.body === null ? null : JSON.parse(String(init.body))
        bodies.push(parsed)
        if (url === `/api/entities/${UUID_A}/tags`) {
          return Promise.resolve(responseOf(200, detailAfterPut))
        }
        return Promise.resolve(responseOf(200, detailBody))
      }),
    )
    const entities = useEntitiesStore()
    await entities.fetchEntityDetail(UUID_A)
    expect(entities.detail?.tags).toEqual(['work'])
    await entities.updateTags(UUID_A, ['work', 'nas'])
    expect(calls).toEqual([`GET /api/entities/${UUID_A}`, `PUT /api/entities/${UUID_A}/tags`])
    expect(bodies[1]).toEqual({ tags: ['work', 'nas'] })
    // PUT応答(操作後の対象状態)でdetailを更新=操作後の再GETを行わない(fetchは2回のみ)
    expect(entities.detail?.tags).toEqual(['work', 'nas'])
    expect(entities.lastTagEditError).toBeNull()
  })

  it('PUT失敗(422 validation_failed)はlastTagEditErrorで示しdetailは旧状態のまま・成功でクリア', async () => {
    const calls: string[] = []
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((input: RequestInfo | URL, init?: RequestInit) => {
        const url = typeof input === 'string' ? input : input.toString()
        calls.push(`${init?.method ?? 'GET'} ${url}`)
        if (url === `/api/entities/${UUID_A}`) {
          return Promise.resolve(responseOf(200, detailBody))
        }
        if (calls.length === 2) {
          return Promise.resolve(
            responseOf(422, {
              error: { code: 'validation_failed', message: 'tag名の規則に違反しています' },
            }),
          )
        }
        return Promise.resolve(responseOf(200, detailAfterPut))
      }),
    )
    const entities = useEntitiesStore()
    await entities.fetchEntityDetail(UUID_A)
    await entities.updateTags(UUID_A, ['BAD TAG'])
    // 失敗時の再GETは行わない・detailは旧状態のまま
    expect(calls).toHaveLength(2)
    expect(entities.lastTagEditError?.code).toBe('validation_failed')
    expect(entities.detail?.tags).toEqual(['work'])
    // 成功でlastTagEditErrorはクリアされdetailは応答で更新される
    await entities.updateTags(UUID_A, ['work', 'nas'])
    expect(entities.lastTagEditError).toBeNull()
    expect(entities.detail?.tags).toEqual(['work', 'nas'])
  })
})

// --- W2レビュー指摘対応(TASK-19): tag編集保存中状態・一覧/tags失効再取得・
//     一覧取得の要求世代・untagged絞り込み ---

// W2レビュー指摘のtest内共通: PUT後の操作後詳細body(tags差し替え)
const detailAfterPutOf = (...tags: string[]) => ({ ...detailBody, tags: [...tags] })

// [covers:frontend.entity-tags-edit-saving-state]
describe('entities storeのtag編集実行中状態(tagEditSaving・W2レビュー指摘高1)', () => {
  it('updateTags実行中はtagEditSaving=true・成功完了でfalseへ戻る', async () => {
    let savingDuringPut = false
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((input: RequestInfo | URL, init?: RequestInit) => {
        const url = typeof input === 'string' ? input : input.toString()
        if (url === `/api/entities/${UUID_A}/tags`) {
          // PUTの応答を返す時点(実行中)の状態を観測する
          savingDuringPut = useEntitiesStore().tagEditSaving
          return Promise.resolve(responseOf(200, detailAfterPutOf('work', 'nas')))
        }
        return Promise.resolve(responseOf(200, detailBody))
      }),
    )
    const entities = useEntitiesStore()
    expect(entities.tagEditSaving).toBe(false)
    await entities.fetchEntityDetail(UUID_A)
    await entities.updateTags(UUID_A, ['work', 'nas'])
    expect(savingDuringPut).toBe(true)
    expect(entities.tagEditSaving).toBe(false)
  })

  it('PUT失敗(422)でもtagEditSavingはfalseへ戻る(trueのまま残らない)', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((input: RequestInfo | URL, init?: RequestInit) => {
        const url = typeof input === 'string' ? input : input.toString()
        if (url === `/api/entities/${UUID_A}/tags`) {
          return Promise.resolve(
            responseOf(422, {
              error: { code: 'validation_failed', message: 'tag名の規則に違反しています' },
            }),
          )
        }
        return Promise.resolve(responseOf(200, detailBody))
      }),
    )
    const entities = useEntitiesStore()
    await entities.fetchEntityDetail(UUID_A)
    await entities.updateTags(UUID_A, ['BAD TAG'])
    expect(entities.lastTagEditError?.code).toBe('validation_failed')
    expect(entities.tagEditSaving).toBe(false)
  })
})

// [covers:frontend.entity-tags-edit-refreshes-list-and-tags]
describe('entities storeのtag編集成功後の失効再取得(W2レビュー指摘中6)', () => {
  it('成功時は一覧を現在の絞り込みで再取得し取得済みのtags選択肢も再取得する', async () => {
    const calls: string[] = []
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((input: RequestInfo | URL, init?: RequestInit) => {
        const url = typeof input === 'string' ? input : input.toString()
        calls.push(`${init?.method ?? 'GET'} ${url}`)
        if (url === `/api/entities/${UUID_A}`) {
          return Promise.resolve(responseOf(200, detailBody))
        }
        if (url === `/api/entities/${UUID_A}/tags`) {
          return Promise.resolve(responseOf(200, detailAfterPutOf('work', 'nas')))
        }
        if (url === '/api/entities?query=beta&tag=work&page=1') {
          return Promise.resolve(responseOf(200, listBody([UUID_A], 1, 1)))
        }
        if (url === '/api/tags') {
          return Promise.resolve(responseOf(200, { tags: [{ tag: 'nas', entity_count: 1 }] }))
        }
        return Promise.reject(new TypeError(`unexpected fetch: ${url}`))
      }),
    )
    const entities = useEntitiesStore()
    const tags = useTagsStore()
    // 一覧(query+tag絞り込み)・tag選択肢・詳細を取得済みの状態にする
    entities.setQuery('beta')
    entities.setTagFilter('work')
    await entities.fetchEntities()
    await tags.fetchTags()
    await entities.fetchEntityDetail(UUID_A)
    calls.length = 0
    await entities.updateTags(UUID_A, ['work', 'nas'])
    // 成功時はPUT後に現在の絞り込みでの一覧再取得とtags選択肢の再取得が続く
    expect(calls).toEqual([
      `PUT /api/entities/${UUID_A}/tags`,
      'GET /api/entities?query=beta&tag=work&page=1',
      'GET /api/tags',
    ])
    expect(tags.items[0]?.tag).toBe('nas')
  })

  it('一覧・tags未取得の時は再取得しない・PUT失敗時も再取得しない', async () => {
    const calls: string[] = []
    let putCount = 0
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((input: RequestInfo | URL, init?: RequestInit) => {
        const url = typeof input === 'string' ? input : input.toString()
        calls.push(`${init?.method ?? 'GET'} ${url}`)
        if (url === `/api/entities/${UUID_A}`) {
          return Promise.resolve(responseOf(200, detailBody))
        }
        if (url === `/api/entities/${UUID_A}/tags`) {
          putCount += 1
          // 1回目のPUTは成功・2回目は422失敗とする
          if (putCount === 1) {
            return Promise.resolve(responseOf(200, detailAfterPutOf('work', 'nas')))
          }
          return Promise.resolve(
            responseOf(422, {
              error: { code: 'validation_failed', message: 'tag名の規則に違反しています' },
            }),
          )
        }
        return Promise.reject(new TypeError(`unexpected fetch: ${url}`))
      }),
    )
    const entities = useEntitiesStore()
    // 一覧・tags選択肢とも未取得(not_fetched)の状態=表示されていないdataのために
    // 取得しない
    await entities.fetchEntityDetail(UUID_A)
    calls.length = 0
    await entities.updateTags(UUID_A, ['work', 'nas'])
    expect(calls).toEqual([`PUT /api/entities/${UUID_A}/tags`])
    // 失敗時も一覧・tagsの再取得は行わない
    await entities.updateTags(UUID_A, ['BAD TAG'])
    expect(calls).toEqual([`PUT /api/entities/${UUID_A}/tags`, `PUT /api/entities/${UUID_A}/tags`])
  })
})

// [covers:frontend.entities-list-request-generation]
describe('entities storeの一覧取得の要求世代(W2レビュー指摘中7)', () => {
  it('並行取得で旧要求の応答が後着しても採用されない(最新要求のみstate更新)', async () => {
    // 1回目(tag=work)の応答のみ保留(gate)とし、2回目(tag=nas)完了後に完了させる
    let releaseWork: () => void = () => {}
    const workGate = new Promise<void>((resolve) => {
      releaseWork = resolve
    })
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((input: RequestInfo | URL) => {
        const url = typeof input === 'string' ? input : input.toString()
        if (url === '/api/entities?tag=work&page=1') {
          return workGate.then(() =>
            // 旧要求は後着の500失敗応答とする(lastError汚染の観測も兼ねる)
            Promise.resolve(
              responseOf(500, { error: { code: 'internal', message: '内部エラーです' } }),
            ),
          )
        }
        if (url === '/api/entities?tag=nas&page=1') {
          return Promise.resolve(responseOf(200, listBody([UUID_B], 2, 5)))
        }
        return Promise.reject(new TypeError(`unexpected fetch: ${url}`))
      }),
    )
    const entities = useEntitiesStore()
    entities.setTagFilter('work')
    const workPromise = entities.fetchEntities()
    // 1回目の応答が未完了のうちに2回目の選択(tag=nas)が発生する
    entities.setTagFilter('nas')
    const nasPromise = entities.fetchEntities()
    await nasPromise
    expect(entities.items.map((item) => item.uuid)).toEqual([UUID_B])
    expect(entities.page).toBe(2)
    expect(entities.totalPages).toBe(5)
    expect(entities.loading).toBe(false)
    expect(entities.lastError).toBeNull()
    // 旧要求(1回目)の応答が後から完了してもstateは2回目の結果のまま
    releaseWork()
    await workPromise
    expect(entities.items.map((item) => item.uuid)).toEqual([UUID_B])
    expect(entities.page).toBe(2)
    expect(entities.totalPages).toBe(5)
    expect(entities.loading).toBe(false)
    // 旧応答の500でlastErrorは設定されない
    expect(entities.lastError).toBeNull()
  })
})

// [covers:frontend.entities-untagged-filter]
describe('entities storeのuntagged絞り込み(W2レビュー指摘中9)', () => {
  it('setUntaggedFilterはuntagged=1&page=1で取得しtagFilterをクリアする(tag=と同時に送らない)', async () => {
    const captured: string[] = []
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((input: RequestInfo | URL) => {
        const url = typeof input === 'string' ? input : input.toString()
        captured.push(url)
        return Promise.resolve(responseOf(200, listBody([], 1, 1)))
      }),
    )
    const entities = useEntitiesStore()
    entities.setTagFilter('work')
    entities.setPage(2)
    entities.setUntaggedFilter()
    expect(entities.untaggedOnly).toBe(true)
    expect(entities.tagFilter).toBeNull()
    expect(entities.page).toBe(1)
    await entities.fetchEntities()
    expect(captured[0]).toBe('/api/entities?untagged=1&page=1')
  })

  it('setTagFilterはuntaggedOnlyをクリアする・setView・refetchEntitiesもクリアする', async () => {
    const captured: string[] = []
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((input: RequestInfo | URL) => {
        const url = typeof input === 'string' ? input : input.toString()
        captured.push(url)
        return Promise.resolve(responseOf(200, listBody([], 1, 1)))
      }),
    )
    const entities = useEntitiesStore()
    // setTagFilterはuntaggedOnlyをクリアする(相互排他)
    entities.setUntaggedFilter()
    entities.setTagFilter('work')
    expect(entities.untaggedOnly).toBe(false)
    await entities.fetchEntities()
    expect(captured[0]).toBe('/api/entities?tag=work&page=1')
    // setViewもuntaggedOnlyをクリアする(他の選択系絞り込みと同一)
    entities.setUntaggedFilter()
    entities.setView('flat')
    expect(entities.untaggedOnly).toBe(false)
    // sync後再取得(refetchEntities)もuntaggedOnlyをクリアする
    entities.setUntaggedFilter()
    await entities.refetchEntities()
    expect(entities.untaggedOnly).toBe(false)
    expect(captured[captured.length - 1]).toBe('/api/entities')
  })
})

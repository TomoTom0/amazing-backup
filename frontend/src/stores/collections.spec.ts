import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createPinia, setActivePinia } from 'pinia'
import { useCollectionsStore } from './collections'
import { useEntitiesStore } from './entities'

// collections store(TASK-17設計2.2・9章W2=TASK-19): collectionビュー=
// collection選択+その構成entity一覧の表示基盤(GET /api/collections=一覧
// {collections:[{id,name,entity_count}]}(entity_count=構成entity数)・
// GET /api/collections/:id=詳細schema{collection:{id,name,uuids}}(POST/PUT
// collections等の書込応答と同一形式=2026-11-14のAPI契約統一裁定・design doc 3.1
// 原則。4.1 collections tableのid/nameが語彙源))。

const UUID_A = '0192a4f0-0000-7000-8000-000000000001'
const UUID_B = '0192a4f0-0000-7000-8000-000000000002'

const collectionsBody = {
  collections: [
    { id: 1, name: 'weekly', entity_count: 3 },
    { id: 2, name: 'nas-移行', entity_count: 5 },
  ],
}

const collectionDetailBody = { collection: { id: 3, name: 'grp', uuids: [UUID_A, UUID_B] } }

/** createCollection成功後の一覧再取得応答(新collection(id=4)を含む) */
const collectionsAfterCreateBody = {
  collections: [...collectionsBody.collections, { id: 4, name: 'new-grp', entity_count: 0 }],
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

// [covers:frontend.collections-client-and-store]
describe('collections store(fetchCollections・fetchCollectionDetail)', () => {
  it('成功時はcollection一覧を保持しlastErrorはnull', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation(() => Promise.resolve(responseOf(200, collectionsBody))),
    )
    const collections = useCollectionsStore()
    await collections.fetchCollections()
    expect(collections.items).toHaveLength(2)
    expect(collections.items[1]?.id).toBe(2)
    expect(collections.items[1]?.name).toBe('nas-移行')
    expect(collections.lastError).toBeNull()
  })

  it('fetchCollectionDetailは/api/collections/<id>の詳細(構成uuid一覧)を保持する', async () => {
    const calls: string[] = []
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((input: RequestInfo | URL, init?: RequestInit) => {
        const url = typeof input === 'string' ? input : input.toString()
        calls.push(`${init?.method ?? 'GET'} ${url}`)
        return Promise.resolve(responseOf(200, collectionDetailBody))
      }),
    )
    const collections = useCollectionsStore()
    await collections.fetchCollectionDetail(3)
    expect(calls).toEqual(['GET /api/collections/3'])
    // uuidsは応答のcollection wrapから取り出す(detailIdは応答のcollection.id)
    expect(collections.detailId).toBe(3)
    expect(collections.detailUuids).toEqual([UUID_A, UUID_B])
    expect(collections.lastError).toBeNull()
  })

  it('失敗時はlastErrorを保持しitems・detailUuidsは空へ戻る(空dataを表示しない)', async () => {
    // 一覧・構成とも保持した状態での失敗=保持済み状態の消去も観測する
    vi.stubGlobal(
      'fetch',
      vi
        .fn()
        .mockImplementationOnce(() => Promise.resolve(responseOf(200, collectionsBody)))
        .mockImplementationOnce(() => Promise.resolve(responseOf(200, collectionDetailBody)))
        .mockImplementationOnce(() =>
          Promise.resolve(
            responseOf(500, { error: { code: 'internal', message: '内部エラーです' } }),
          ),
        )
        .mockImplementationOnce(() =>
          Promise.resolve(
            responseOf(404, { error: { code: 'not_found', message: 'collectionが見つかりません' } }),
          ),
        ),
    )
    const collections = useCollectionsStore()
    await collections.fetchCollections()
    await collections.fetchCollectionDetail(3)
    expect(collections.items).toHaveLength(2)
    expect(collections.detailUuids).toHaveLength(2)
    // 3回目(一覧の再取得)が失敗すると一覧は空へ戻る
    await collections.fetchCollections()
    expect(collections.items).toEqual([])
    expect(collections.lastError?.code).toBe('internal')
    // 4回目(構成の再取得)の404失敗では構成uuid一覧が空へ戻る
    await collections.fetchCollectionDetail(3)
    expect(collections.detailUuids).toEqual([])
    expect(collections.lastError?.code).toBe('not_found')
  })
})

// [covers:frontend.view-options-fetch-state]
describe('collections storeの取得状態(fetchState・W2レビュー指摘中8)', () => {
  it('未取得=not_fetched・失敗=error後の再fetchでokへ復帰する(再試行導線)', async () => {
    vi.stubGlobal(
      'fetch',
      vi
        .fn()
        .mockImplementationOnce(() =>
          Promise.resolve(
            responseOf(500, { error: { code: 'internal', message: '内部エラーです' } }),
          ),
        )
        .mockImplementationOnce(() => Promise.resolve(responseOf(200, collectionsBody))),
    )
    const collections = useCollectionsStore()
    expect(collections.fetchState).toBe('not_fetched')
    await collections.fetchCollections()
    expect(collections.fetchState).toBe('error')
    await collections.fetchCollections()
    expect(collections.fetchState).toBe('ok')
    expect(collections.lastError).toBeNull()
  })
})

// [covers:frontend.collections-write-client-and-store]
describe('collections storeの書込action(W2レビュー指摘高3)', () => {
  it('createCollectionは成功時 構成stateへ保持し一覧を再取得・作成されたcollectionを選択する', async () => {
    const calls: string[] = []
    const bodies: unknown[] = []
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((input: RequestInfo | URL, init?: RequestInit) => {
        const url = typeof input === 'string' ? input : input.toString()
        calls.push(`${init?.method ?? 'GET'} ${url}`)
        if (init?.body !== undefined) bodies.push(JSON.parse(String(init.body)))
        if (url === '/api/collections' && init?.method === 'POST') {
          return Promise.resolve(
            responseOf(200, { collection: { id: 4, name: 'new-grp', uuids: [] } }),
          )
        }
        if (url === '/api/collections') {
          return Promise.resolve(responseOf(200, collectionsAfterCreateBody))
        }
        if (url === '/api/entities?collection=4&page=1') {
          return Promise.resolve(responseOf(200, { entities: [], page: 1, total_pages: 1 }))
        }
        return Promise.reject(new TypeError(`unexpected fetch: ${url}`))
      }),
    )
    const collections = useCollectionsStore()
    const entities = useEntitiesStore()
    const ok = await collections.createCollection('new-grp')
    expect(ok).toBe(true)
    expect(calls).toEqual(['POST /api/collections', 'GET /api/collections', 'GET /api/entities?collection=4&page=1'])
    expect(bodies).toEqual([{ name: 'new-grp' }])
    // 構成stateは作成応答・一覧は再取得結果(新collection含む)・選択は新collectionへ
    expect(collections.detailId).toBe(4)
    expect(collections.detailUuids).toEqual([])
    expect(collections.items).toHaveLength(3)
    expect(entities.collectionFilter).toBe(4)
    expect(collections.lastWriteError).toBeNull()
    expect(collections.writing).toBe(false)
  })

  it('renameCollectionは成功時 一覧を再取得し選択中の構成stateを応答へ更新する', async () => {
    const calls: string[] = []
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((input: RequestInfo | URL, init?: RequestInit) => {
        const url = typeof input === 'string' ? input : input.toString()
        calls.push(`${init?.method ?? 'GET'} ${url}`)
        if (url === '/api/collections/3' && init?.method === 'PUT') {
          return Promise.resolve(responseOf(200, collectionDetailBody))
        }
        if (url === '/api/collections/3') {
          return Promise.resolve(responseOf(200, collectionDetailBody))
        }
        if (url === '/api/collections') {
          return Promise.resolve(responseOf(200, collectionsBody))
        }
        return Promise.reject(new TypeError(`unexpected fetch: ${url}`))
      }),
    )
    const collections = useCollectionsStore()
    const entities = useEntitiesStore()
    // 選択中(id=3)の構成を取得済みの状態にする
    await collections.fetchCollections()
    entities.setCollectionFilter(3)
    await collections.fetchCollectionDetail(3)
    calls.length = 0
    const ok = await collections.renameCollection(3, 'renamed')
    expect(ok).toBe(true)
    expect(calls).toEqual(['PUT /api/collections/3', 'GET /api/collections'])
    expect(collections.detailId).toBe(3)
    expect(collections.detailUuids).toEqual([UUID_A, UUID_B])
    expect(collections.lastWriteError).toBeNull()
  })

  it('deleteCollectionは成功時 一覧を再取得し選択中ならcollectionFilterを解除して一覧再取得', async () => {
    const calls: string[] = []
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((input: RequestInfo | URL, init?: RequestInit) => {
        const url = typeof input === 'string' ? input : input.toString()
        calls.push(`${init?.method ?? 'GET'} ${url}`)
        if (url === '/api/collections/3' && init?.method === 'DELETE') {
          return Promise.resolve(responseOf(200, { status: 'ok' }))
        }
        if (url === '/api/collections') {
          return Promise.resolve(responseOf(200, { collections: [{ id: 1, name: 'weekly', entity_count: 3 }] }))
        }
        if (url === '/api/entities?page=1') {
          return Promise.resolve(responseOf(200, { entities: [], page: 1, total_pages: 1 }))
        }
        return Promise.reject(new TypeError(`unexpected fetch: ${url}`))
      }),
    )
    const collections = useCollectionsStore()
    const entities = useEntitiesStore()
    await collections.fetchCollections()
    entities.setCollectionFilter(3)
    calls.length = 0
    const ok = await collections.deleteCollection(3)
    expect(ok).toBe(true)
    expect(calls).toEqual(['DELETE /api/collections/3', 'GET /api/collections', 'GET /api/entities?page=1'])
    // 選択中collectionの削除=選択解除(絞り込み無し)と構成state失効
    expect(entities.collectionFilter).toBeNull()
    expect(collections.detailId).toBeNull()
    expect(collections.detailUuids).toEqual([])
    expect(collections.lastWriteError).toBeNull()
  })

  it('addCollectionItemsは成功時 構成uuid一覧を応答へ更新し一覧を再取得する', async () => {
    const calls: string[] = []
    const bodies: unknown[] = []
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((input: RequestInfo | URL, init?: RequestInit) => {
        const url = typeof input === 'string' ? input : input.toString()
        calls.push(`${init?.method ?? 'GET'} ${url}`)
        if (init?.body !== undefined) bodies.push(JSON.parse(String(init.body)))
        if (url === '/api/collections/3/items' && init?.method === 'POST') {
          return Promise.resolve(responseOf(200, collectionDetailBody))
        }
        if (url === '/api/collections') {
          return Promise.resolve(responseOf(200, collectionsBody))
        }
        return Promise.reject(new TypeError(`unexpected fetch: ${url}`))
      }),
    )
    const collections = useCollectionsStore()
    const ok = await collections.addCollectionItems(3, [UUID_A, UUID_B])
    expect(ok).toBe(true)
    expect(calls).toEqual(['POST /api/collections/3/items', 'GET /api/collections'])
    expect(bodies).toEqual([{ uuids: [UUID_A, UUID_B] }])
    expect(collections.detailId).toBe(3)
    expect(collections.detailUuids).toEqual([UUID_A, UUID_B])
  })

  it('removeCollectionItemは成功時 構成uuid一覧を応答(操作後の対象状態)へ更新する', async () => {
    const calls: string[] = []
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((input: RequestInfo | URL, init?: RequestInit) => {
        const url = typeof input === 'string' ? input : input.toString()
        calls.push(`${init?.method ?? 'GET'} ${url}`)
        if (url === `/api/collections/3/items/${UUID_A}` && init?.method === 'DELETE') {
          return Promise.resolve(
            responseOf(200, { collection: { id: 3, name: 'grp', uuids: [UUID_B] } }),
          )
        }
        if (url === '/api/collections') {
          return Promise.resolve(responseOf(200, collectionsBody))
        }
        return Promise.reject(new TypeError(`unexpected fetch: ${url}`))
      }),
    )
    const collections = useCollectionsStore()
    const ok = await collections.removeCollectionItem(3, UUID_A)
    expect(ok).toBe(true)
    expect(calls).toEqual([`DELETE /api/collections/3/items/${UUID_A}`, 'GET /api/collections'])
    expect(collections.detailUuids).toEqual([UUID_B])
  })

  it('書込失敗(409 duplicate)はlastWriteErrorで示しwritingはfalseへ戻る(構成stateは失敗前のまま)', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((input: RequestInfo | URL, init?: RequestInit) => {
        const url = typeof input === 'string' ? input : input.toString()
        if (url === '/api/collections' && init?.method === 'POST') {
          return Promise.resolve(
            responseOf(409, {
              error: { code: 'duplicate', message: '同名のcollectionが既に存在します' },
            }),
          )
        }
        return Promise.reject(new TypeError(`unexpected fetch: ${url}`))
      }),
    )
    const collections = useCollectionsStore()
    const ok = await collections.createCollection('weekly')
    expect(ok).toBe(false)
    expect(collections.lastWriteError?.code).toBe('duplicate')
    expect(collections.writing).toBe(false)
    expect(collections.detailId).toBeNull()
    expect(collections.items).toEqual([])
  })
})

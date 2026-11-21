import { afterEach, describe, expect, it, vi } from 'vitest'
import {
  addCollectionItems,
  createCollection,
  deleteCollection,
  fetchCollectionDetail,
  fetchCollections,
  isCollectionDetailResponse,
  isCollectionsResponse,
  removeCollectionItem,
  renameCollection,
} from './client'

// GET /api/collections・GET /api/collections/:id応答の型ガードとcollections client
// (TASK-17設計3.2(GET /api/collections=collection一覧・GET /api/collections/:id=
// 詳細schema{collection:{id,name,uuids}}=構成uuid一覧)・4.1(collections tableの
// id/name)・2.2(collectionビュー=collection選択+その構成entity一覧)・9章W2=TASK-19)。
// 一覧要素のentity_count(構成entity数)と:id応答の詳細schema(POST/PUT collections・
// items追加の書込応答と同一形式)は2026-11-14のAPI契約統一裁定(design doc 3.1
// 「読取系と同一schema」原則)へ統一済み。

const UUID_A = '0192a4f0-0000-7000-8000-000000000001'
const UUID_B = '0192a4f0-0000-7000-8000-000000000002'

const collectionsBody = {
  collections: [
    { id: 1, name: 'weekly', entity_count: 3 },
    { id: 2, name: 'nas-移行', entity_count: 5 },
  ],
}

const collectionDetailBody = {
  collection: { id: 3, name: 'grp', uuids: [UUID_A, UUID_B] },
}

const responseOf = (status: number, body: unknown): Response =>
  new Response(JSON.stringify(body), {
    status,
    headers: { 'Content-Type': 'application/json' },
  })

afterEach(() => {
  vi.unstubAllGlobals()
})

// [covers:frontend.collections-client-and-store]
describe('GET /api/collections応答の型ガード(isCollectionsResponse)', () => {
  it('形式一致のみtrue(collections配列+各要素のid/name/entity_count)', () => {
    expect(isCollectionsResponse(collectionsBody)).toBe(true)
    expect(isCollectionsResponse({ collections: [] })).toBe(true)
    // collections欠落・非配列・要素のfield欠落・型不一致・nullはfalse
    expect(isCollectionsResponse({})).toBe(false)
    expect(isCollectionsResponse({ collections: 'x' })).toBe(false)
    expect(isCollectionsResponse({ collections: [{ id: 1 }] })).toBe(false)
    expect(isCollectionsResponse({ collections: [{ id: '1', name: 'weekly', entity_count: 3 }] })).toBe(false)
    // 契約統一裁定: entity_count(構成entity数)は必須
    expect(
      isCollectionsResponse({ collections: [{ id: 1, name: 'weekly' }] }),
    ).toBe(false)
    expect(
      isCollectionsResponse({ collections: [{ id: 1, name: 'weekly', entity_count: '3' }] }),
    ).toBe(false)
    expect(isCollectionsResponse(null)).toBe(false)
  })
})

// [covers:frontend.collections-client-and-store]
describe('GET /api/collections/:id応答の型ガード(isCollectionDetailResponse)', () => {
  it('形式一致のみtrue(collection wrap+各fieldのid/name/uuids)', () => {
    expect(isCollectionDetailResponse(collectionDetailBody)).toBe(true)
    expect(isCollectionDetailResponse({ collection: { id: 1, name: 'weekly', uuids: [] } })).toBe(true)
    // collection欠落・collection非object・uuidsのみの旧形式・field欠落・型不一致・nullはfalse
    expect(isCollectionDetailResponse({})).toBe(false)
    expect(isCollectionDetailResponse({ collection: 'x' })).toBe(false)
    expect(isCollectionDetailResponse({ uuids: [UUID_A] })).toBe(false)
    expect(
      isCollectionDetailResponse({ collection: { name: 'weekly', uuids: [UUID_A] } }),
    ).toBe(false)
    expect(isCollectionDetailResponse({ collection: { id: 1, uuids: [UUID_A] } })).toBe(false)
    expect(isCollectionDetailResponse({ collection: { id: 1, name: 'weekly' } })).toBe(false)
    expect(
      isCollectionDetailResponse({ collection: { id: '1', name: 'weekly', uuids: [UUID_A] } }),
    ).toBe(false)
    expect(
      isCollectionDetailResponse({ collection: { id: 1, name: 'weekly', uuids: [UUID_A, 2] } }),
    ).toBe(false)
    expect(isCollectionDetailResponse(null)).toBe(false)
  })
})

// [covers:frontend.collections-client-and-store]
describe('collections client(fetchCollections・fetchCollectionDetail)', () => {
  it('GET /api/collections・GET /api/collections/:idへ向かい応答を返す', async () => {
    const calls: string[] = []
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((input: RequestInfo | URL, init?: RequestInit) => {
        const url = typeof input === 'string' ? input : input.toString()
        calls.push(`${init?.method ?? 'GET'} ${url}`)
        if (url === '/api/collections/3') {
          return Promise.resolve(responseOf(200, collectionDetailBody))
        }
        return Promise.resolve(responseOf(200, collectionsBody))
      }),
    )
    const list = await fetchCollections()
    expect(calls[0]).toBe('GET /api/collections')
    expect(list.ok).toBe(true)
    if (list.ok) {
      expect(list.data.collections).toHaveLength(2)
      expect(list.data.collections[1]?.name).toBe('nas-移行')
    }

    const detail = await fetchCollectionDetail(3)
    expect(calls[1]).toBe('GET /api/collections/3')
    expect(detail.ok).toBe(true)
    if (detail.ok) {
      // 応答は詳細schema{collection:{id,name,uuids}}
      expect(detail.data.collection.id).toBe(3)
      expect(detail.data.collection.name).toBe('grp')
      expect(detail.data.collection.uuids).toEqual([UUID_A, UUID_B])
    }
  })

  it('不在collectionの404 not_foundはerror扱い', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation(() =>
        Promise.resolve(
          responseOf(404, { error: { code: 'not_found', message: 'collectionが見つかりません' } }),
        ),
      ),
    )
    const result = await fetchCollectionDetail(99)
    expect(result.ok).toBe(false)
    if (!result.ok) {
      expect(result.error.code).toBe('not_found')
      expect(result.httpStatus).toBe(404)
    }
  })
})

// [covers:frontend.collections-write-client-and-store]
describe('collections書込client(W2レビュー指摘高3・2026-11-14API契約)', () => {
  it('createCollectionはPOST /api/collectionsへ{name}bodyで呼び詳細schema応答を返す', async () => {
    const calls: string[] = []
    const bodies: unknown[] = []
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((input: RequestInfo | URL, init?: RequestInit) => {
        const url = typeof input === 'string' ? input : input.toString()
        calls.push(`${init?.method ?? 'GET'} ${url}`)
        bodies.push(init?.body === undefined ? null : JSON.parse(String(init.body)))
        return Promise.resolve(
          responseOf(200, { collection: { id: 4, name: 'new-grp', uuids: [] } }),
        )
      }),
    )
    const result = await createCollection('new-grp')
    expect(calls).toEqual(['POST /api/collections'])
    expect(bodies).toEqual([{ name: 'new-grp' }])
    expect(result.ok).toBe(true)
    if (result.ok) {
      expect(result.data.collection.id).toBe(4)
      expect(result.data.collection.name).toBe('new-grp')
      expect(result.data.collection.uuids).toEqual([])
    }
  })

  it('renameCollectionはPUT /api/collections/:idへ{name}bodyで呼ぶ', async () => {
    const calls: string[] = []
    const bodies: unknown[] = []
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((input: RequestInfo | URL, init?: RequestInit) => {
        const url = typeof input === 'string' ? input : input.toString()
        calls.push(`${init?.method ?? 'GET'} ${url}`)
        bodies.push(init?.body === undefined ? null : JSON.parse(String(init.body)))
        return Promise.resolve(responseOf(200, collectionDetailBody))
      }),
    )
    const result = await renameCollection(3, 'renamed')
    expect(calls).toEqual(['PUT /api/collections/3'])
    expect(bodies).toEqual([{ name: 'renamed' }])
    expect(result.ok).toBe(true)
    if (result.ok) {
      expect(result.data.collection.uuids).toEqual([UUID_A, UUID_B])
    }
  })

  it('deleteCollectionはDELETE /api/collections/:idを呼び{status:"ok"}応答を検証する', async () => {
    const calls: string[] = []
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((input: RequestInfo | URL, init?: RequestInit) => {
        const url = typeof input === 'string' ? input : input.toString()
        calls.push(`${init?.method ?? 'GET'} ${url}`)
        return Promise.resolve(responseOf(200, { status: 'ok' }))
      }),
    )
    const result = await deleteCollection(3)
    expect(calls).toEqual(['DELETE /api/collections/3'])
    expect(result.ok).toBe(true)
    if (result.ok) {
      expect(result.data.status).toBe('ok')
    }
  })

  it('addCollectionItemsはPOST /api/collections/:id/itemsへ{uuids}bodyで呼ぶ', async () => {
    const calls: string[] = []
    const bodies: unknown[] = []
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((input: RequestInfo | URL, init?: RequestInit) => {
        const url = typeof input === 'string' ? input : input.toString()
        calls.push(`${init?.method ?? 'GET'} ${url}`)
        bodies.push(init?.body === undefined ? null : JSON.parse(String(init.body)))
        return Promise.resolve(responseOf(200, collectionDetailBody))
      }),
    )
    const result = await addCollectionItems(3, [UUID_A, UUID_B])
    expect(calls).toEqual(['POST /api/collections/3/items'])
    expect(bodies).toEqual([{ uuids: [UUID_A, UUID_B] }])
    expect(result.ok).toBe(true)
    if (result.ok) {
      expect(result.data.collection.uuids).toEqual([UUID_A, UUID_B])
    }
  })

  it('removeCollectionItemはDELETE /api/collections/:id/items/:uuidを呼ぶ', async () => {
    const calls: string[] = []
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((input: RequestInfo | URL, init?: RequestInit) => {
        const url = typeof input === 'string' ? input : input.toString()
        calls.push(`${init?.method ?? 'GET'} ${url}`)
        return Promise.resolve(responseOf(200, collectionDetailBody))
      }),
    )
    const result = await removeCollectionItem(3, UUID_A)
    expect(calls).toEqual([`DELETE /api/collections/3/items/${UUID_A}`])
    expect(result.ok).toBe(true)
    if (result.ok) {
      // 除去後の構成(応答=操作後の対象状態)を返す
      expect(result.data.collection.uuids).toEqual([UUID_A, UUID_B])
    }
  })

  it('書込のerror応答(409 duplicate)はerror扱い', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation(() =>
        Promise.resolve(
          responseOf(409, {
            error: { code: 'duplicate', message: '同名のcollectionが既に存在します' },
          }),
        ),
      ),
    )
    const result = await createCollection('weekly')
    expect(result.ok).toBe(false)
    if (!result.ok) {
      expect(result.error.code).toBe('duplicate')
      expect(result.httpStatus).toBe(409)
    }
  })
})

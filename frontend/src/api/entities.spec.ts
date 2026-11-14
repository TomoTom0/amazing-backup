import { afterEach, describe, expect, it, vi } from 'vitest'
import {
  fetchEntities,
  fetchEntityDetail,
  fetchEntityVersions,
  isEntitiesResponse,
  isEntityDetailResponse,
} from './client'

// GET /api/entities・:uuid・:uuid/versions応答の型ガードとentities client
// (TASK-9設計6章・7章・コードレビュー指摘高4=EntitiesView/EntityDetailViewの
// 本格画面)。実描画は実browser観測のため条件書frontend.tomlのexcluded参照
// (UI誘導状態はstores/entities.tsのgetterで担保)。
// W2(TASK-17設計2.2・3.2・9章W2=TASK-19)分の追記: EntitySummaryへtags配列・
// EntityDetailへcollections配列(所属collection表示の基盤)・fetchEntitiesの
// tag/collection絞り込みparam。実装前のため追記分の期待値(配列必須の否定case・
// param直列化)は実行failを許容(実装後に緑化)。

const summary = {
  uuid: '0192a4f0-0000-7000-8000-000000000001',
  title: 'a.zip',
  locations: [
    {
      source_path: '/data/a.zip',
      category: 'docs',
      label: 'work',
      identity_hash: 'sha256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=',
      last_backup: '2026-11-05T12:00:00+09:00',
    },
  ],
  versions: 2,
  latest_date: '2026-11-05',
  latest_size: 1024,
  pins: [{ pin: 'default', date: '2026-11-05', source_size: 1024 }],
  tags: ['work'],
}

const detailBody = {
  ...summary,
  collections: ['weekly'],
  version_list: [
    {
      date: '2026-11-05',
      version_ts: '2026-11-05T12:00:00Z',
      version_id: 'vid-2',
      batch_id: 'batch-2',
      source_size: 1024,
    },
    {
      date: '2026-11-04',
      version_ts: '2026-11-04T12:00:00Z',
      version_id: 'vid-1',
      batch_id: 'batch-1',
      source_size: 512,
    },
  ],
}

const versionsBody = {
  uuid: summary.uuid,
  title: summary.title,
  locations: summary.locations,
  versions: [
    {
      date: '2026-11-05',
      version_ts: '2026-11-05T12:00:00Z',
      version_id: 'vid-2',
      source_size: 1024,
      state: '現行',
    },
    {
      date: '2026-11-04',
      version_ts: '2026-11-04T12:00:00Z',
      version_id: 'vid-1',
      source_size: 512,
      state: 'noncurrent',
    },
  ],
  pins: summary.pins,
}

const responseOf = (status: number, body: unknown): Response =>
  new Response(JSON.stringify(body), {
    status,
    headers: { 'Content-Type': 'application/json' },
  })

afterEach(() => {
  vi.unstubAllGlobals()
})

// [covers:frontend.entities-client]
describe('GET /api/entities応答の型ガード', () => {
  it('応答形式(entities配列+page+total_pages+要素schema)を判定する', () => {
    expect(isEntitiesResponse({ entities: [summary], page: 1, total_pages: 1 })).toBe(true)
    expect(isEntitiesResponse({ entities: [], page: 1, total_pages: 1 })).toBe(true)
    // page・total_pagesの型不一致はfalse
    expect(isEntitiesResponse({ entities: [], page: '1', total_pages: 1 })).toBe(false)
    expect(isEntitiesResponse({ entities: [], page: 1, total_pages: null })).toBe(false)
    // 要素schemaの不一致(title・versions件数・locations形式)はfalse
    expect(
      isEntitiesResponse({ entities: [{ ...summary, title: 1 }], page: 1, total_pages: 1 }),
    ).toBe(false)
    expect(
      isEntitiesResponse({ entities: [{ ...summary, versions: '2' }], page: 1, total_pages: 1 }),
    ).toBe(false)
    expect(
      isEntitiesResponse({
        entities: [{ ...summary, locations: '/data/a.zip' }],
        page: 1,
        total_pages: 1,
      }),
    ).toBe(false)
    // W2: 要素のtags(tag列chip表示の基盤)は配列が必須
    expect(
      isEntitiesResponse({ entities: [{ ...summary, tags: 'work' }], page: 1, total_pages: 1 }),
    ).toBe(false)
    // entities欠落・nullはfalse
    expect(isEntitiesResponse({ page: 1, total_pages: 1 })).toBe(false)
    expect(isEntitiesResponse(null)).toBe(false)
  })
})

// [covers:frontend.entities-client]
describe('GET /api/entities/:uuid応答の型ガード(isEntityDetailResponse)', () => {
  it('詳細形式(要約+version_list+W2のtags/collections)を判定する', () => {
    expect(isEntityDetailResponse(detailBody)).toBe(true)
    // W2: tags(一覧要素と共通)・collections(所属collection表示の基盤)は配列が必須
    expect(isEntityDetailResponse({ ...detailBody, tags: 'work' })).toBe(false)
    expect(isEntityDetailResponse({ ...detailBody, collections: 'weekly' })).toBe(false)
    expect(isEntityDetailResponse({ ...detailBody, collections: ['weekly', 1] })).toBe(false)
    // version_list形式の不一致・欠落はfalse(既存規則)
    expect(isEntityDetailResponse({ ...detailBody, version_list: 'x' })).toBe(false)
    expect(isEntityDetailResponse(summary)).toBe(false)
  })
})

// [covers:frontend.entities-client]
describe('entities clientの取得(fetchEntities/detail/versions)', () => {
  it('fetchEntitiesは値のある絞り込みparamのみURL queryへ直列化する', async () => {
    const captured: string[] = []
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((input: RequestInfo | URL) => {
        captured.push(typeof input === 'string' ? input : input.toString())
        return Promise.resolve(responseOf(200, { entities: [], page: 1, total_pages: 1 }))
      }),
    )
    // param無しはquery無しのURLへ
    await fetchEntities()
    // query/category/label/pageはそれぞれURL queryへ(順序は問わない)
    await fetchEntities({ query: 'beta', category: 'docs', label: 'work', page: 2 })
    // 値の無いparamは付けない(pageのみの単独指定)
    await fetchEntities({ page: 3 })
    expect(captured[0]).toBe('/api/entities')
    expect(captured[1].startsWith('/api/entities?')).toBe(true)
    expect(captured[1]).toContain('query=beta')
    expect(captured[1]).toContain('category=docs')
    expect(captured[1]).toContain('label=work')
    expect(captured[1]).toContain('page=2')
    expect(captured[2]).toBe('/api/entities?page=3')
  })

  // [covers:frontend.entities-tag-collection-filter]
  it('fetchEntitiesはtag/collection絞り込みparamも値のあるもののみURL queryへ直列化する', async () => {
    const captured: string[] = []
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((input: RequestInfo | URL) => {
        captured.push(typeof input === 'string' ? input : input.toString())
        return Promise.resolve(responseOf(200, { entities: [], page: 1, total_pages: 1 }))
      }),
    )
    // 変数経由で渡す(実装前の型定義に無いparamも実行時の直列化として観測する)
    const params = { query: 'beta', tag: 'work', collection: '3', page: 2 }
    await fetchEntities(params)
    expect(captured[0]).toContain('query=beta')
    expect(captured[0]).toContain('tag=work')
    expect(captured[0]).toContain('collection=3')
    expect(captured[0]).toContain('page=2')
    // 値の無いtag(空文字)・collection(undefined)は付けない
    const noFilters = { tag: '', collection: undefined, page: 1 }
    await fetchEntities(noFilters)
    expect(captured[1]).toBe('/api/entities?page=1')
  })

  it('fetchEntityDetailは/api/entities/<uuid>を呼び不在404はerror扱いにする', async () => {
    const captured: string[] = []
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((input: RequestInfo | URL) => {
        captured.push(typeof input === 'string' ? input : input.toString())
        if (captured.length === 1) {
          return Promise.resolve(responseOf(200, detailBody))
        }
        return Promise.resolve(
          responseOf(404, { error: { code: 'not_found', message: '対象のentityが見つかりません' } }),
        )
      }),
    )
    const ok = await fetchEntityDetail(summary.uuid)
    expect(ok.ok).toBe(true)
    if (ok.ok) {
      expect(ok.data.uuid).toBe(summary.uuid)
      expect(ok.data.version_list.length).toBe(2)
      // W2: 詳細応答はtags(tag編集の対象表示)・collections(所属collection表示)を含む
      expect(ok.data.tags).toEqual(['work'])
      expect(ok.data.collections).toEqual(['weekly'])
    }
    const missing = await fetchEntityDetail('99999999-9999-9999-9999-999999999999')
    expect(missing.ok).toBe(false)
    if (!missing.ok) {
      expect(missing.error.code).toBe('not_found')
      expect(missing.httpStatus).toBe(404)
    }
    expect(captured[0]).toBe(`/api/entities/${summary.uuid}`)
    expect(captured[1]).toBe('/api/entities/99999999-9999-9999-9999-999999999999')
  })

  it('fetchEntityVersionsは/api/entities/<uuid>/versionsを呼びstate付きの版一覧を返す', async () => {
    const captured: string[] = []
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((input: RequestInfo | URL) => {
        captured.push(typeof input === 'string' ? input : input.toString())
        return Promise.resolve(responseOf(200, versionsBody))
      }),
    )
    const result = await fetchEntityVersions(summary.uuid)
    expect(result.ok).toBe(true)
    if (result.ok) {
      expect(result.data.versions.length).toBe(2)
      expect(result.data.versions[0].state).toBe('現行')
      expect(result.data.versions[1].state).toBe('noncurrent')
    }
    expect(captured[0]).toBe(`/api/entities/${summary.uuid}/versions`)
  })

  // [covers:frontend.entities-untagged-filter]
  it('fetchEntitiesはuntagged(true)をuntagged=1へ直列化する(false・未指定は付けない)', async () => {
    const captured: string[] = []
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((input: RequestInfo | URL) => {
        captured.push(typeof input === 'string' ? input : input.toString())
        return Promise.resolve(responseOf(200, { entities: [], page: 1, total_pages: 1 }))
      }),
    )
    await fetchEntities({ untagged: true, page: 1 })
    expect(captured[0]).toContain('untagged=1')
    expect(captured[0]).toContain('page=1')
    // falseは付けない(tag=との同時指定がserver側400のためtag側は値のある時のみ)
    await fetchEntities({ untagged: false, page: 1 })
    expect(captured[1]).toBe('/api/entities?page=1')
  })

  // [covers:frontend.folders-labelless-node]
  it('fetchEntitiesはlabelの空文字も直列化する(ラベルなし絞り込み=label=)', async () => {
    const captured: string[] = []
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((input: RequestInfo | URL) => {
        captured.push(typeof input === 'string' ? input : input.toString())
        return Promise.resolve(responseOf(200, { entities: [], page: 1, total_pages: 1 }))
      }),
    )
    // labelは空文字(ラベルなし完全一致)も送る=値のあるparamのみ規則のlabel限定例外
    await fetchEntities({ label: '', page: 1 })
    expect(captured[0]).toBe('/api/entities?label=&page=1')
  })
})

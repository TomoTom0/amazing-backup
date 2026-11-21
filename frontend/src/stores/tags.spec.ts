import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createPinia, setActivePinia } from 'pinia'
import { useTagsStore } from './tags'

// tags store(TASK-17設計2.2・9章W2=TASK-19): tagビューのtag選択肢一覧
// (GET /api/tags={tags:[{tag,entity_count}]})の表示基盤。実装前のため
// src/stores/tags.tsは未実装=本fileのtestは実行failを許容(実装後に緑化)。

const tagsBody = {
  tags: [
    { tag: 'work', entity_count: 2 },
    { tag: 'nas', entity_count: 1 },
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

// [covers:frontend.tags-client-and-store]
describe('tags store(fetchTags)', () => {
  it('成功時はtag一覧を保持しlastErrorはnull', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation(() => Promise.resolve(responseOf(200, tagsBody))),
    )
    const tags = useTagsStore()
    await tags.fetchTags()
    expect(tags.items).toHaveLength(2)
    expect(tags.items[0]?.tag).toBe('work')
    expect(tags.items[0]?.entity_count).toBe(2)
    expect(tags.lastError).toBeNull()
  })

  it('失敗時はlastErrorを保持しitemsは空へ戻る(空dataを表示しない)', async () => {
    // 非空一覧を保持した状態での再取得失敗=保持済みitemsの消去も観測する
    vi.stubGlobal(
      'fetch',
      vi
        .fn()
        .mockImplementationOnce(() => Promise.resolve(responseOf(200, tagsBody)))
        .mockImplementationOnce(() =>
          Promise.resolve(
            responseOf(500, { error: { code: 'internal', message: '内部エラーです' } }),
          ),
        ),
    )
    const tags = useTagsStore()
    await tags.fetchTags()
    expect(tags.items).toHaveLength(2)
    await tags.fetchTags()
    expect(tags.items).toEqual([])
    expect(tags.lastError?.code).toBe('internal')
  })
})

// [covers:frontend.view-options-fetch-state]
describe('tags storeの取得状態(fetchState・W2レビュー指摘中8)', () => {
  it('未取得=not_fetched・失敗=errorを遷移し失敗後の再fetchでokへ復帰する(再試行導線)', async () => {
    vi.stubGlobal(
      'fetch',
      vi
        .fn()
        .mockImplementationOnce(() =>
          Promise.resolve(
            responseOf(500, { error: { code: 'internal', message: '内部エラーです' } }),
          ),
        )
        .mockImplementationOnce(() => Promise.resolve(responseOf(200, tagsBody))),
    )
    const tags = useTagsStore()
    expect(tags.fetchState).toBe('not_fetched')
    await tags.fetchTags()
    // 一度失敗してもfetchStateはerrorであり再呼出で再取得できる(項目数判定でなく
    // 状態で取得要否を判定する=lastError===null条件に阻まれない)
    expect(tags.fetchState).toBe('error')
    await tags.fetchTags()
    expect(tags.fetchState).toBe('ok')
    expect(tags.items).toHaveLength(2)
    expect(tags.lastError).toBeNull()
  })
})

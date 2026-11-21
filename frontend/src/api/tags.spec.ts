import { afterEach, describe, expect, it, vi } from 'vitest'
import { fetchTags, isTagsResponse } from './client'

// GET /api/tags応答の型ガードとtags client(TASK-17設計3.2(GET /api/tags=
// {tags:[{tag,entity_count}]})・2.2(tagビュー=/tag=でtag選択+tag付きentity一覧・
// tag無しも選択可)・9章W2=TAG-19)。実装前のためclient.tsへtags系は未実装=
// 本fileのtestは実行failを許容(実装後に緑化)。

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

afterEach(() => {
  vi.unstubAllGlobals()
})

// [covers:frontend.tags-client-and-store]
describe('GET /api/tags応答の型ガード(isTagsResponse)', () => {
  it('形式一致のみtrue(tags配列+各要素のtag/entity_count)', () => {
    expect(isTagsResponse(tagsBody)).toBe(true)
    expect(isTagsResponse({ tags: [] })).toBe(true)
    // tags欠落・tags非配列・要素のfield欠落・型不一致・nullはfalse
    expect(isTagsResponse({})).toBe(false)
    expect(isTagsResponse({ tags: 'x' })).toBe(false)
    expect(isTagsResponse({ tags: [{ tag: 'work' }] })).toBe(false)
    expect(isTagsResponse({ tags: [{ tag: 'work', entity_count: '2' }] })).toBe(false)
    expect(isTagsResponse({ tags: [{ tag: 1, entity_count: 2 }] })).toBe(false)
    expect(isTagsResponse(null)).toBe(false)
  })
})

// [covers:frontend.tags-client-and-store]
describe('tags client(fetchTags)', () => {
  it('GET /api/tagsへ向かい200応答のtag一覧を返す', async () => {
    const calls: string[] = []
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((input: RequestInfo | URL, init?: RequestInit) => {
        const url = typeof input === 'string' ? input : input.toString()
        calls.push(`${init?.method ?? 'GET'} ${url}`)
        return Promise.resolve(responseOf(200, tagsBody))
      }),
    )
    const result = await fetchTags()
    expect(calls).toEqual(['GET /api/tags'])
    expect(result.ok).toBe(true)
    if (result.ok) {
      expect(result.data.tags).toHaveLength(2)
      expect(result.data.tags[0]?.tag).toBe('work')
      expect(result.data.tags[0]?.entity_count).toBe(2)
    }
  })

  it('401 unauthenticatedはerror扱い', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation(() =>
        Promise.resolve(
          responseOf(401, { error: { code: 'unauthenticated', message: '認証が必要です' } }),
        ),
      ),
    )
    const result = await fetchTags()
    expect(result.ok).toBe(false)
    if (!result.ok) {
      expect(result.error.code).toBe('unauthenticated')
      expect(result.httpStatus).toBe(401)
    }
  })
})

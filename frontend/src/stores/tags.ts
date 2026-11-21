import { defineStore } from 'pinia'
import { fetchTags, type ApiError, type TagItem } from '../api/client'

/** 選択肢(tag/folder/collection一覧)の取得状態(W2レビュー指摘中8=TAG-19:
 *  items空+lastError===null判定では失敗後に再試行できないため状態を明示する) */
export type OptionsFetchState = 'not_fetched' | 'ok' | 'error'

/**
 * tags store(TASK-17設計2.2・9章W2): tagビューのtag選択肢一覧
 * (GET /api/tags={tags:[{tag,entity_count}]})の表示基盤。取得失敗時はitemsを
 * 空へ戻しlastErrorを保持する(空dataを表示しない=fail-closedと共通)。
 */
export const useTagsStore = defineStore('tags', {
  state: () => ({
    /** tag一覧(未取得・取得失敗時は空) */
    items: [] as TagItem[],
    /** 取得状態(失敗後もfetchTagsの再呼出で再取得できる=Viewの再試行button基盤) */
    fetchState: 'not_fetched' as OptionsFetchState,
    lastError: null as ApiError | null,
  }),
  actions: {
    /** tag一覧の取得(失敗時はitemsを空へ戻しlastErrorを保持) */
    async fetchTags(): Promise<void> {
      const result = await fetchTags()
      if (result.ok) {
        this.items = result.data.tags
        this.fetchState = 'ok'
        this.lastError = null
        return
      }
      this.items = []
      this.fetchState = 'error'
      this.lastError = result.error
    },
  },
})

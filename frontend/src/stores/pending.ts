import { defineStore } from 'pinia'
import {
  fetchPending,
  postPendingScan,
  type ApiError,
  type PendingItem,
} from '../api/client'

/**
 * pending catalog store(TASK-9設計7章・12章P5): アップロード前カタログの一覧保持・
 * status別集計・scan実行。PendingView(未backup/changed/unknown一覧+scanボタン)の
 * 基盤。取得失敗時はlastErrorを保持し一覧を空へ戻す(空dataを表示しない)。
 */
export const usePendingStore = defineStore('pending', {
  state: () => ({
    /** pending_items一覧(未取得・失敗時は空) */
    items: [] as PendingItem[],
    lastError: null as ApiError | null,
    /** scan実行中状態(POST /api/pending/scan+一覧再取得の間) */
    scanning: false,
  }),
  getters: {
    /** status別件数の集計(PendingViewの未backup/changed/unknown一覧のUI状態) */
    counts(): { unbacked: number; changed: number; ok: number; unknown: number } {
      const counts = { unbacked: 0, changed: 0, ok: 0, unknown: 0 }
      for (const item of this.items) {
        if (
          item.status === 'unbacked' ||
          item.status === 'changed' ||
          item.status === 'ok' ||
          item.status === 'unknown'
        ) {
          counts[item.status] += 1
        }
      }
      return counts
    },
  },
  actions: {
    /** 一覧の取得(失敗時はitemsを空へ戻しlastErrorを保持) */
    async fetchPending(): Promise<void> {
      const result = await fetchPending()
      if (result.ok) {
        this.items = result.data.pending
        this.lastError = null
        return
      }
      this.items = []
      this.lastError = result.error
    },
    /** scan実行(POST後に一覧を再取得してitemsを更新=手動トリガのUI基盤) */
    async runScan(): Promise<void> {
      this.scanning = true
      const result = await postPendingScan()
      if (!result.ok) {
        this.lastError = result.error
      }
      await this.fetchPending()
      this.scanning = false
    },
  },
})

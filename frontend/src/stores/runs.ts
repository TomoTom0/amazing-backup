import { defineStore } from 'pinia'
import { fetchRuns, type ApiError, type RunItem } from '../api/client'

/**
 * runs store(TASK-17設計2.1・9章W1): ホームbackup概况=前回backup(直近runの日時・
 * 対象数)の表示基盤。latestRun getterは降順応答の先頭要素を直近runとして返す
 * (server.runs-listの並び解釈に整合。一覧空時はnull)。取得失敗時はlastErrorを保持
 * しitemsを空へ戻す(空dataを表示しない)。
 */
export const useRunsStore = defineStore('runs', {
  state: () => ({
    /** run一覧(date降順。未取得・取得失敗時は空) */
    items: [] as RunItem[],
    lastError: null as ApiError | null,
  }),
  getters: {
    /** 直近run=降順応答の先頭要素(前回backup表示。一覧空時はnull) */
    latestRun(): RunItem | null {
      return this.items[0] ?? null
    },
  },
  actions: {
    /** run一覧の取得(失敗時はitemsを空へ戻しlastErrorを保持) */
    async fetchRuns(): Promise<void> {
      const result = await fetchRuns()
      if (result.ok) {
        this.items = result.data.runs
        this.lastError = null
        return
      }
      this.items = []
      this.lastError = result.error
    },
  },
})

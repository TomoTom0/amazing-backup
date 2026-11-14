import { defineStore } from 'pinia'
import { fetchJobs, type ApiError, type JobItem } from '../api/client'

/**
 * jobs store(TASK-17設計2.1・5.3・9章W1): ホーム実行中job区画・転送タブのW1基盤。
 * W1のserver応答は常に空配列(server.jobs-list)。W1ではpolling(setInterval等の
 * 自動更新)を行わずfetchJobsは手動呼出のみとする(設計5.3のpollingはW4の転送
 * タブ実装で追加)。取得失敗時はlastErrorを保持しitemsを空へ戻す(空dataを
 * 表示しない)。
 */
export const useJobsStore = defineStore('jobs', {
  state: () => ({
    /** job一覧(未取得・取得失敗時は空) */
    items: [] as JobItem[],
    lastError: null as ApiError | null,
  }),
  actions: {
    /** job一覧の取得(失敗時はitemsを空へ戻しlastErrorを保持) */
    async fetchJobs(): Promise<void> {
      const result = await fetchJobs()
      if (result.ok) {
        this.items = result.data.jobs
        this.lastError = null
        return
      }
      this.items = []
      this.lastError = result.error
    },
  },
})

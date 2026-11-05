import { defineStore } from 'pinia'
import { fetchDiff, postDiffRun, type ApiError, type DiffResponse } from '../api/client'

/**
 * diff store(TASK-9設計7章・12章P6=diff UI): diff結果の保持・diff実行・DiffViewの
 * UI誘導状態。取得失敗時はlastErrorを保持しresultをnullへ戻す(空dataを表示しない)。
 * 未実行diffの404 not_foundは未実行(not_run)扱いとし、未認証401等の他の失敗
 * (error)とUI状態として区別する。
 */
export const useDiffStore = defineStore('diff', {
  state: () => ({
    /** 最終diff結果(未取得・失敗時はnull) */
    result: null as DiffResponse | null,
    lastError: null as ApiError | null,
    /** diff実行中状態(POST /api/diffの間) */
    running: false,
  }),
  getters: {
    /** DiffViewのUI誘導状態: 実行中=running・結果保持済み=result・未実行(未取得・
     * または未実行diffの404 not_found)=not_run・その他の失敗(未認証401等)=error */
    diffView(): 'running' | 'result' | 'not_run' | 'error' {
      if (this.running) return 'running'
      if (this.result) return 'result'
      if (this.lastError) {
        return this.lastError.code === 'not_found' ? 'not_run' : 'error'
      }
      return 'not_run'
    },
    /** 結果の集計(diff結果表示の枠。未取得時はすべて0) */
    summary(): { entries: number; missing: number; unexpected: number } {
      return {
        entries: this.result?.entries ?? 0,
        missing: this.result?.missing.length ?? 0,
        unexpected: this.result?.unexpected.length ?? 0,
      }
    },
  },
  actions: {
    /** 最終diff結果の取得(失敗時はresultをnullへ戻しlastErrorを保持) */
    async fetchDiff(): Promise<void> {
      const result = await fetchDiff()
      if (result.ok) {
        this.result = result.data
        this.lastError = null
        return
      }
      this.result = null
      this.lastError = result.error
    },
    /** diff実行(POSTの応答はdiff結果そのもののため実行後の再GETは行わず
     * POST応答をresultへ保持する=diff実行ボタンのUI基盤) */
    async runDiff(): Promise<void> {
      this.running = true
      const result = await postDiffRun()
      if (result.ok) {
        this.result = result.data
        this.lastError = null
      } else {
        this.lastError = result.error
      }
      this.running = false
    },
  },
})

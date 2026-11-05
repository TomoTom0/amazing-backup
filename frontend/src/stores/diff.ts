import { defineStore } from 'pinia'
import { fetchDiff, postDiffRun, type ApiError, type DiffResponse } from '../api/client'

/**
 * diff store(TASK-9設計7章・12章P6=diff UI): diff結果の保持・diff実行・DiffViewの
 * UI誘導状態。取得失敗時はlastErrorを保持しresultをnullへ戻す(空dataを表示しない)。
 * 未実行diffの404 not_foundは未実行(not_run)扱いとし、未認証401等の他の失敗
 * (error)とUI状態として区別する。実行(POST)の失敗はlastRunErrorへ別保持し
 * 旧resultは保持する=旧結果表示中も直前の実行失敗を明示できる。
 */
export const useDiffStore = defineStore('diff', {
  state: () => ({
    /** 最終diff結果(未取得・取得失敗時はnull。実行失敗時は旧結果を保持) */
    result: null as DiffResponse | null,
    lastError: null as ApiError | null,
    /** 直近のdiff実行(POST /api/diff)のerror(成功でクリア。実行失敗では
     * 旧resultを残すためlastErrorとは別に保持する) */
    lastRunError: null as ApiError | null,
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
    /** 直近のdiff実行(POST)の失敗有無(旧resultの有無に関係なくViewで案内する=
     * 保存済み結果が無い初回実行の失敗も表示から隠さない) */
    hasRunError(): boolean {
      return this.lastRunError !== null
    },
    /** 旧結果を表示している状態での直近実行失敗の有無(Viewの明示行用) */
    showingStaleResult(): boolean {
      return this.result !== null && this.lastRunError !== null
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
     * POST応答をresultへ保持する=diff実行ボタンのUI基盤。失敗時は旧resultを
     * 残したままlastRunErrorへ保持=表示側で「旧結果+今回の失敗」を明示する) */
    async runDiff(): Promise<void> {
      this.running = true
      const result = await postDiffRun()
      if (result.ok) {
        this.result = result.data
        this.lastError = null
        this.lastRunError = null
      } else {
        this.lastRunError = result.error
      }
      this.running = false
    },
  },
})

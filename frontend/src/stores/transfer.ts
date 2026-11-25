import { defineStore } from 'pinia'
import type { ApiError } from '../api/client'
import { useJobsStore } from './jobs'

/**
 * 転送タブの取得form状態(TASK-17設計2.3・9章W4)。prefillは
 * /transfer?get=<uuid>&version_id=...のquery param由来(2.2詳細画面の版行の
 * 「取得」button導線=job発行までは転送タブで確認)。submitはjobs storeの
 * issueGetへ接続する(server側検証を正本としつつ・uuid/outの空欄は投入前に
 * client側で案内する)。upload form(push)はW5実装のため本storeには含めない。
 */
export const useTransferStore = defineStore('transfer', {
  state: () => ({
    /** 対象uuid(prefill対応) */
    uuid: '',
    /** 版のversion_id(空=最新) */
    versionId: '',
    /** ピン留め版のpin名(空=指定なし) */
    pin: '',
    /** 出力dir(server側path・必須) */
    out: '',
    /** tier(Standard/Bulk・既定Standard=CLI getと同一) */
    tier: 'Standard',
    /** days(既定7=CLI getと同一) */
    days: 7,
    /** sha不一致の既存平文への上書き(getの--yes相当・既定false) */
    overwrite: false,
    /** prefill適用済みflag(同一対象の再適用をしない=手動入力をprefillで上書きしない) */
    prefillApplied: false,
    /** prefill適用済みの対象key(<uuid>改行<version_id>)。別対象の明示選択では再適用する */
    prefillKey: null as string | null,
    /** 発行実行中(Viewは発行buttonを無効化) */
    submitting: false,
    /** client側検証の案内文(server検証とは別の簡易前置き) */
    clientError: null as string | null,
    /** 発行失敗(jobs storeのlastIssueErrorの転写。server側422等) */
    lastSubmitError: null as ApiError | null,
  }),
  actions: {
    /** route queryからのprefill(?get=<uuid>&version_id=...)。適用済みと同一の
     *  対象は再適用しない(同一画面での手動入力をprefillで上書きしない)が、
     *  別対象の明示選択(詳細画面の取得button)は新しい対象で再適用する。
     *  getが無いqueryでは適用しない=詳細画面の取得button導線は必ずgetを伴う) */
    prefillFromQuery(query: Record<string, string | undefined>): void {
      const target = query['get']
      if (!target) return
      const versionId = query['version_id'] ?? ''
      const key = `${target}\n${versionId}`
      if (this.prefillApplied && this.prefillKey === key) return
      this.prefillApplied = true
      this.prefillKey = key
      this.uuid = target
      this.versionId = versionId
      // pinはserver側でversion_idより優先されるため、前回form入力のpinが
      // 残ると明示選択した版ではなくピン留め版が取得されてしまう
      this.pin = ''
    },
    /** formのリセット(既定値へ戻す) */
    reset(): void {
      this.uuid = ''
      this.versionId = ''
      this.pin = ''
      this.out = ''
      this.tier = 'Standard'
      this.days = 7
      this.overwrite = false
      this.prefillApplied = false
      this.prefillKey = null
      this.clientError = null
      this.lastSubmitError = null
    },
    /** 取得jobの発行(uuid/out必須のclient側最低限検証→jobs storeのissueGet)。
     *  戻り値=発行成否 */
    async submitGet(): Promise<boolean> {
      if (this.uuid.trim() === '' || this.out.trim() === '') {
        this.clientError = '対象uuidと出力dirを指定してください'
        return false
      }
      this.clientError = null
      this.submitting = true
      const jobs = useJobsStore()
      const ok = await jobs.issueGet({
        uuid: this.uuid.trim(),
        version_id: this.versionId.trim() === '' ? undefined : this.versionId.trim(),
        pin: this.pin.trim() === '' ? undefined : this.pin.trim(),
        out: this.out.trim(),
        tier: this.tier,
        days: this.days,
        overwrite: this.overwrite,
      })
      this.submitting = false
      this.lastSubmitError = jobs.lastIssueError
      return ok
    },
  },
})

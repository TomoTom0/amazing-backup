import { defineStore } from 'pinia'
import {
  postPushDryRun,
  type ApiError,
  type PushDryRunResponse,
  type PushTargetInput,
} from '../api/client'
import { useJobsStore } from './jobs'

export type UploadTargetRow = {
  path: string
  title: string
}

/** path末尾のbasenameから初期title候補を導出する(拡張子を除いたbasename。
 *  正本2.2「path・title入力済み」の導線用。64文字上限・空や区切りのみは空文字) */
export function defaultTitleFromPath(rawPath: string): string {
  const parts = rawPath.split('/').filter((s) => s.length > 0)
  const base = parts[parts.length - 1] ?? ''
  const dot = base.lastIndexOf('.')
  const stem = dot > 0 ? base.slice(0, dot) : base
  return Array.from(stem).slice(0, 64).join('')
}

/** 入力行から送信用のPushTargetInput配列を組む(title空欄はnull扱い=送信時に省略) */
function normalizeUploadTargets(rows: UploadTargetRow[]): PushTargetInput[] {
  return rows.map((r) => {
    const path = r.path.trim()
    const title = r.title.trim()
    return title === '' ? { path } : { path, title }
  })
}

/** targets比較用の正規化signature(並び・path・title完全一致) */
function signatureOfTargets(targets: PushTargetInput[]): string {
  return JSON.stringify(targets)
}

/** client側の最低限検証(server検証の前置き=get formのclientErrorと同一構成)。
 *  対象のpath空欄・重複を案内する。問題なしはnull */
function validateUploadTargets(targets: PushTargetInput[]): string | null {
  if (targets.length === 0 || targets.some((t) => t.path === '')) {
    return 'upload対象のserver側pathを入力してください'
  }
  const seen = new Set<string>()
  for (const t of targets) {
    if (seen.has(t.path)) {
      return 'upload対象のpathが重複しています'
    }
    seen.add(t.path)
  }
  return null
}

/**
 * 転送タブの取得form・upload form状態(TASK-17設計2.2・2.3・6章・9章W4/W5・TASK-28設計D-P4)。
 * - 取得form: ?get=<uuid>&version_id=... prefill + submitGet
 * - upload form(W5): 対象path+title列挙(?upload=<path> prefill) →
 *   runDryRun(POST /api/push/dry-runで予測+実効profile/bucket/prefix/recipients指紋を
 *   取得=接続先確認) → 2段階確認(ViewのTwoStepConfirm部品) → submitPush(確認済み時のみ
 *   POST /api/jobs/push)
 * - dry-run確認の失効: 対象rowsの変更(編集・行追加削除)・prefill再適用でdry-run結果と
 *   確認済み状態を「dry-run未実行」へ戻す。dry-run応答は入力世代と照合し古い応答は破棄する
 */
export const useTransferStore = defineStore('transfer', {
  state: () => ({
    // --- 取得form(W4) ---
    uuid: '',
    versionId: '',
    pin: '',
    out: '',
    tier: 'Standard',
    days: 7,
    overwrite: false,
    prefillApplied: false,
    prefillKey: null as string | null,
    submitting: false,
    clientError: null as string | null,
    lastSubmitError: null as ApiError | null,

    // --- upload form(W5) ---
    uploadTargets: [{ path: '', title: '' }] as UploadTargetRow[],
    uploadPrefillApplied: false,
    uploadPrefillKey: null as string | null,
    /** 入力世代(対象変更・dry-run要求ごとに増加。応答受領時の世代が現在と不一致なら
     *  古い応答として破棄する=連打・編集直後の後着対策) */
    uploadGeneration: 0,
    dryRunning: false,
    dryRunResult: null as PushDryRunResponse | null,
    lastDryRunError: null as ApiError | null,
    /** dry-run確認済みtargets(確認時点のsnapshot。発行時の完全一致検査に使う) */
    confirmedTargets: null as PushTargetInput[] | null,
    uploadSubmitting: false,
    uploadClientError: null as string | null,
    lastUploadSubmitError: null as ApiError | null,
  }),
  getters: {
    /** 現在のuploadTargetsがdry-run確認済みtargets(snapshot)と完全一致しているか
     *  (6章の接続先確認ガード) */
    isUploadConfirmed: (state): boolean =>
      state.dryRunResult !== null &&
      state.confirmedTargets !== null &&
      signatureOfTargets(state.confirmedTargets) ===
        signatureOfTargets(normalizeUploadTargets(state.uploadTargets)),
  },
  actions: {
    /** route queryからのprefill(?get=<uuid>&version_id=... および ?upload=<path>) */
    prefillFromQuery(query: Record<string, string | undefined>): void {
      const target = query['get']
      if (target) {
        const versionId = query['version_id'] ?? ''
        const key = `${target}\n${versionId}`
        if (!(this.prefillApplied && this.prefillKey === key)) {
          this.prefillApplied = true
          this.prefillKey = key
          this.uuid = target
          this.versionId = versionId
          this.pin = ''
        }
      }
      const uploadPath = query['upload']
      if (uploadPath) {
        if (!(this.uploadPrefillApplied && this.uploadPrefillKey === uploadPath)) {
          this.uploadPrefillApplied = true
          this.uploadPrefillKey = uploadPath
          this.uploadTargets = [{ path: uploadPath, title: defaultTitleFromPath(uploadPath) }]
          this.uploadClientError = null
          this.lastUploadSubmitError = null
          // prefill再適用でもdry-run確認を失効させる
          this.invalidateUploadConfirm()
        }
      }
    },
    /** formのリセット(取得・upload双方を既定値へ戻す) */
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

      this.uploadTargets = [{ path: '', title: '' }]
      this.uploadPrefillApplied = false
      this.uploadPrefillKey = null
      this.dryRunning = false
      this.lastDryRunError = null
      this.uploadSubmitting = false
      this.uploadClientError = null
      this.lastUploadSubmitError = null
      this.invalidateUploadConfirm()
    },
    /** 取得jobの発行(W4) */
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
    /** dry-run確認の失効(dry-run結果と確認済み状態を「dry-run未実行」へ戻す。2段階確認
     *  buttonはdry-run結果と一緒に表示されるため、View部品ごと取り除かれてarmed状態も
     *  解除される) */
    invalidateUploadConfirm(): void {
      this.uploadGeneration++
      this.dryRunResult = null
      this.confirmedTargets = null
    },
    /** upload対象行の追加(確認は失効する) */
    addUploadTarget(): void {
      this.uploadTargets.push({ path: '', title: '' })
      this.invalidateUploadConfirm()
    },
    /** upload対象行の削除(最後の1行は空行へ戻す。確認は失効する) */
    removeUploadTarget(index: number): void {
      if (index < 0 || index >= this.uploadTargets.length) return
      if (this.uploadTargets.length === 1) {
        this.uploadTargets = [{ path: '', title: '' }]
      } else {
        this.uploadTargets.splice(index, 1)
      }
      this.invalidateUploadConfirm()
    },
    /** upload対象行の更新(path・title編集で確認は失効する) */
    updateUploadTarget(index: number, patch: Partial<UploadTargetRow>): void {
      const row = this.uploadTargets[index]
      if (!row) return
      if (patch.path !== undefined) row.path = patch.path
      if (patch.title !== undefined) row.title = patch.title
      this.invalidateUploadConfirm()
    },
    /** 接続先確認状態の明示解除(取り消しbutton用=dry-run未実行へ戻す) */
    cancelUploadConfirm(): void {
      this.invalidateUploadConfirm()
    },
    /** dry-run予測+接続先情報の取得(成功で当該targetsを確認済み状態へ進める) */
    async runDryRun(): Promise<boolean> {
      const normalized = normalizeUploadTargets(this.uploadTargets)
      const invalid = validateUploadTargets(normalized)
      if (invalid !== null) {
        this.uploadClientError = invalid
        return false
      }
      this.uploadClientError = null
      this.lastUploadSubmitError = null
      // 要求中は前の確認結果を使えなくする
      this.dryRunResult = null
      this.confirmedTargets = null
      this.dryRunning = true
      const generation = ++this.uploadGeneration
      const result = await postPushDryRun({ targets: normalized })
      if (generation !== this.uploadGeneration) {
        // 古い応答(連打・編集直後の後着)は破棄する
        return false
      }
      this.dryRunning = false
      if (result.ok) {
        this.dryRunResult = result.data
        this.lastDryRunError = null
        this.confirmedTargets = normalized
        return true
      }
      this.lastDryRunError = result.error
      return false
    },
    /** upload(push)jobの発行(6章: dry-runでの接続先確認済みでなければ発行しない) */
    async submitPush(): Promise<boolean> {
      if (!this.isUploadConfirmed || this.confirmedTargets === null) {
        this.uploadClientError = '先にdry-runで予測と接続先を確認してください'
        return false
      }
      const normalized = normalizeUploadTargets(this.uploadTargets)
      // 送信targetsがdry-run確認済みtargets(確認時点のsnapshot)と完全一致することを検査
      if (signatureOfTargets(normalized) !== signatureOfTargets(this.confirmedTargets)) {
        this.invalidateUploadConfirm()
        this.uploadClientError = '対象がdry-run確認時から変わっています。再度確認してください'
        return false
      }
      this.uploadClientError = null
      this.uploadSubmitting = true
      const jobs = useJobsStore()
      const ok = await jobs.issuePush({ targets: normalized })
      this.uploadSubmitting = false
      this.lastUploadSubmitError = jobs.lastIssueError
      if (ok) {
        // 発行済み(再発行には再度のdry-run確認を要求する)
        this.confirmedTargets = null
      }
      return ok
    },
  },
})

import { defineStore } from 'pinia'
import {
  fetchJobDetail,
  fetchJobs,
  postJobCancel,
  postJobsGet,
  postJobsPush,
  type ApiError,
  type GetJobRequest,
  type JobItem,
  type PushJobRequest,
  type PushJobResultFile,
} from '../api/client'
import { useUiRefreshStore } from './ui-refresh'

/** 進行中jobのstatus語彙(設計3.2: ホーム実行中job区画・nav dot・activeJobsの基準) */
export const ACTIVE_JOB_STATUSES: readonly string[] = [
  'queued',
  'running',
  'waiting_restore',
]

/** push jobの結果から失敗対象行(status==='failed')のみを抽出する(設計5.5・9章W5)。
 *  push job以外・結果未確定時は空 */
export function failedPushItems(job: JobItem): PushJobResultFile[] {
  if (job.kind !== 'push' || job.result === null) return []
  return job.result.files.filter((file) => file.status === 'failed')
}

/** push jobの結果行(対象単位。job行の下にinline展開するdata源。設計D-P4) */
export function pushJobFiles(job: JobItem): PushJobResultFile[] {
  if (job.kind !== 'push' || job.result === null) return []
  return job.result.files
}

/** job一覧・ホーム実行中job区画の対象列表示(getはuuid・pushは先頭path+件数) */
export function jobTargetSummary(job: JobItem): string {
  if (job.kind === 'get') {
    return job.params?.uuid ?? '-'
  }
  const targets = job.params?.targets
  if (!targets || targets.length === 0) return '-'
  const first = targets[0]?.path ?? '-'
  return targets.length === 1 ? first : `${first} (+${targets.length - 1}件)`
}

/**
 * jobs store(TASK-17設計2.1・2.3・5.3・9章W4/W5): ホーム実行中job区画・転送タブの
 * job一覧・取得/upload jobの発行・再実行・取消・polling制御。pollingは設計5.3(転送タブ・
 * ホームの実行中job追従。間隔はui-refresh storeのintervalMs既定10秒・無効可)。job詳細の
 * 取得失敗・発行失敗・取消失敗はそれぞれ別状態(detailError/lastIssueError/
 * lastCancelError)で保持する(P5 lastScanError・P6 lastRunErrorと同一構成)。
 */
export const useJobsStore = defineStore('jobs', {
  state: () => ({
    /** job一覧(未取得・取得失敗時は空。並びはserver側=進行中優先・id降順) */
    items: [] as JobItem[],
    lastError: null as ApiError | null,
    /** 選択jobの詳細(GET /api/jobs/:id) */
    detail: null as JobItem | null,
    detailError: null as ApiError | null,
    /** 取得・upload job発行の実行中状態(Viewは発行buttonを無効化) */
    issuing: false,
    lastIssueError: null as ApiError | null,
    /** 取消失敗の別状態(実行中は409=取消できない案内の基盤) */
    lastCancelError: null as ApiError | null,
    /** pollingの世代(startPolling毎に増加=旧loopの停止判定。0=未起動) */
    pollingEpoch: 0,
    /** polling利用箇所の参照数(App.vueのnav dot用・転送タブ・ホームの表示中追従が
     *  独立にstart/stopできる=参照カウント。0になった時点でpollingを停止する) */
    pollingHolders: 0,
  }),
  getters: {
    /** 進行中(queued/running/waiting_restore)のjob一覧(ホーム実行中job区画・nav dot) */
    activeJobs: (state): JobItem[] =>
      state.items.filter((j) => ACTIVE_JOB_STATUSES.includes(j.status)),
    /** 実行中jobの有無(navの転送tab status dot・ホーム区画の表示条件) */
    hasActiveJobs(): boolean {
      return this.activeJobs.length > 0
    },
  },
  actions: {
    /** job一覧の取得(失敗時はitemsを空へ戻しlastErrorを保持=空dataを表示しない) */
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
    /** job詳細の取得(不在id等はdetailError。成功でdetailを更新) */
    async fetchJobDetail(id: number): Promise<void> {
      const result = await fetchJobDetail(id)
      if (result.ok) {
        this.detail = result.data.job
        this.detailError = null
        return
      }
      this.detail = null
      this.detailError = result.error
    },
    /** 取得jobの発行(発行後は一覧を再取得=転送タブのjob一覧へ即時反映。
     *  失敗(422等)はlastIssueErrorで保持し一覧は変更しない) */
    async issueGet(params: GetJobRequest): Promise<boolean> {
      this.issuing = true
      const result = await postJobsGet(params)
      this.issuing = false
      if (result.ok) {
        this.lastIssueError = null
        await this.fetchJobs()
        return true
      }
      this.lastIssueError = result.error
      return false
    },
    /** upload(push)jobの発行(POST /api/jobs/push→一覧再取得) */
    async issuePush(params: PushJobRequest): Promise<boolean> {
      this.issuing = true
      const result = await postJobsPush(params)
      this.issuing = false
      if (result.ok) {
        this.lastIssueError = null
        await this.fetchJobs()
        return true
      }
      this.lastIssueError = result.error
      return false
    },
    /** get jobの再実行(同一paramsで再発行=冪等。失敗・interrupted・取消済みjobの
     *  再実行buttonから呼ばれる) */
    async rerunGet(job: JobItem): Promise<boolean> {
      if (job.kind !== 'get' || job.params === null) {
        this.lastIssueError = { code: 'internal', message: 'jobのparamsが読み取れません' }
        return false
      }
      const params = job.params
      return this.issueGet({
        uuid: params.uuid,
        version_id: params.version_id ?? undefined,
        pin: params.pin ?? undefined,
        out: params.out,
        tier: params.tier,
        days: params.days,
        overwrite: params.overwrite,
      })
    },
    /** push jobの再実行(同一targetsで再発行=成功済み対象はskipされ冪等) */
    async rerunPush(job: JobItem): Promise<boolean> {
      if (job.kind !== 'push' || job.params === null) {
        this.lastIssueError = { code: 'internal', message: 'jobのparamsが読み取れません' }
        return false
      }
      return this.issuePush({
        targets: job.params.targets.map((t) =>
          t.title === null ? { path: t.path } : { path: t.path, title: t.title },
        ),
      })
    },
    /** kindに応じたjob再実行(get/push共通の再実行button入口) */
    async rerunJob(job: JobItem): Promise<boolean> {
      if (job.kind === 'push') {
        return this.rerunPush(job)
      }
      return this.rerunGet(job)
    },
    /** queued jobの取消(成功時一覧再取得。実行中・完了済みの409はlastCancelError) */
    async cancelJob(id: number): Promise<void> {
      const result = await postJobCancel(id)
      if (result.ok) {
        this.lastCancelError = null
        await this.fetchJobs()
        return
      }
      this.lastCancelError = result.error
    },
    /** polling開始(設計5.3: ui-refreshのintervalMs間隔でfetchJobs。参照カウント=
     *  App.vueのnav dot用と転送タブ・ホームの表示中追従が独立にstart/stopできる。
     *  最初の要求でloopを1本だけ起動し・間隔は毎tick読み直す=設定変更は次tickから
     *  反映される) */
    startPolling(): void {
      this.pollingHolders++
      if (this.pollingHolders === 1) {
        const epoch = ++this.pollingEpoch
        this.scheduleNextPoll(epoch)
      }
    },
    /** polling停止(参照を1つ解放。最後の参照でloopを停止する=進行中の待機timerの
     *  発火後に停止) */
    stopPolling(): void {
      if (this.pollingHolders > 0) {
        this.pollingHolders--
      }
      if (this.pollingHolders === 0) {
        this.pollingEpoch++
      }
    },
    /** 次tickの予約(無効(null)時はfetchせず1秒後に設定変更を再確認する) */
    scheduleNextPoll(epoch: number): void {
      const interval = useUiRefreshStore().intervalMs
      const delay = interval === null ? 1000 : interval
      setTimeout(() => {
        void this.runPollTick(epoch)
      }, delay)
    },
    /** pollingの1tick(世代不一致=停止済みloopは何もしない) */
    async runPollTick(epoch: number): Promise<void> {
      if (epoch !== this.pollingEpoch) return
      const interval = useUiRefreshStore().intervalMs
      if (interval !== null) {
        await this.fetchJobs()
      }
      if (epoch !== this.pollingEpoch) return
      this.scheduleNextPoll(epoch)
    },
  },
})

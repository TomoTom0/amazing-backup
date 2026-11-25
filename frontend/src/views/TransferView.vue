<script setup lang="ts">
import { computed, onMounted, onUnmounted } from 'vue'
import { useRoute } from 'vue-router'
import { useJobsStore } from '../stores/jobs'
import { useTransferStore } from '../stores/transfer'

// TASK-17設計2.3・9章W4(TASK-27): 転送タブ=取得form+job一覧+polling。
// 取得form(uuid・版(省略=最新・version_id/pin指定)・出力dir・tier・days・
// overwrite)はtransfer storeが持ち、詳細画面の版行「取得」からの導線
// (?get=<uuid>&version_id=...)をprefillする(job発行までは転送タブで確認=2.2)。
// job一覧はjobs store(GET /api/jobs。pollingは設計5.3・ui-refreshの間隔既定10秒)。
// upload form(push)はW5実装。再実行button(冪等)・取消button(queuedのみ)を
// 進行中/完了行に置く(実行中の取消は409=完了まで実行のため表示しない)。
// 実描画の観測は実browser(条件書frontend.tomlのexcluded参照。form値のprefill・
// 発行・取消・再実行の成否はtransfer/jobs storeの状態で担保)。

const route = useRoute()
const jobs = useJobsStore()
const transfer = useTransferStore()

/** route queryのstring化(?get=&version_id=。重複時は先頭) */
const queryString = (key: string): string | undefined => {
  const value = route.query[key]
  if (typeof value === 'string') return value
  if (Array.isArray(value)) {
    const first = value.find((v): v is string => typeof v === 'string')
    return first
  }
  return undefined
}

/** route queryからget formをprefill(1度だけ=手動入力を上書きしない) */
const prefill = (): void => {
  transfer.prefillFromQuery({
    get: queryString('get'),
    version_id: queryString('version_id'),
  })
}

/** 実行中jobがある間のjob一覧自動更新(設計5.3: 転送タブ表示中はpolling) */
onMounted(() => {
  prefill()
  void jobs.fetchJobs()
  jobs.startPolling()
})

/** polling停止(転送タブを離れる時。nav dot用のpolling(App.vue)は認証状態側で
 *  start/stopするため互いに独立=ここでは表示中の追従を停止するのみ) */
onUnmounted(() => {
  jobs.stopPolling()
})

/** job一覧表示の並びはserver応答(進行中優先・id降順)のまま */
const jobRows = computed(() => jobs.items)

/** 取消できるのはqueuedのみ(実行中は409=完了まで実行) */
const cancellable = (status: string): boolean => status === 'queued'

/** 再実行は完了系(job.paramsが無いと再発行できない) */
const rerunnable = (status: string): boolean =>
  ['succeeded', 'failed', 'interrupted', 'cancelled'].includes(status)

/** job状態の表示語彙(status dotのst-* classと同一語彙) */
const statusLabel: Record<string, string> = {
  queued: '待機中(queued)',
  running: '実行中(running)',
  waiting_restore: '復元待ち(waiting_restore)',
  succeeded: '完了(succeeded)',
  failed: '失敗(failed)',
  interrupted: '中断(interrupted)',
  cancelled: '取り消し済み(cancelled)',
}

/** 進行の表示(N/M件。progressが無い行は未開始扱いで表示しない) */
const progressText = (progress: { done: number; total: number } | null): string => {
  if (progress === null) return '-'
  return `${progress.done}/${progress.total}`
}

/** get formのsubmit(jobs storeのissueGetへ) */
const submitGet = (): void => {
  void transfer.submitGet()
}
</script>

<template>
  <section class="card">
    <h2>取得</h2>
    <p class="muted">
      backup対象のfileをserver側で取得します(DEEP_ARCHIVE対象は復元job発行後に
      復元を待って自動で取得します)。取得したfileはserver上の出力dirへ配置されます。
    </p>
    <p v-if="transfer.clientError" class="message">{{ transfer.clientError }}</p>
    <p v-if="transfer.lastSubmitError" class="message">
      取得jobの発行に失敗しました({{ transfer.lastSubmitError.code }})。uuid・版・
      出力dir・tier・daysの指定を確認してください。
    </p>
    <p v-if="transfer.submitting" class="muted">取得jobを発行中...</p>
    <form class="transfer-get-form" @submit.prevent="submitGet">
      <label>
        対象uuid
        <input
          v-model="transfer.uuid"
          type="text"
          name="uuid"
          :disabled="transfer.submitting"
        />
      </label>
      <label>
        版version_id(省略=最新)
        <input
          v-model="transfer.versionId"
          type="text"
          name="version_id"
          :disabled="transfer.submitting"
        />
      </label>
      <label>
        pin名(省略可)
        <input
          v-model="transfer.pin"
          type="text"
          name="pin"
          :disabled="transfer.submitting"
        />
      </label>
      <label>
        出力dir(server側path)
        <input
          v-model="transfer.out"
          type="text"
          name="out"
          :disabled="transfer.submitting"
        />
      </label>
      <label>
        tier
        <select v-model="transfer.tier" name="tier" :disabled="transfer.submitting">
          <option value="Standard">Standard</option>
          <option value="Bulk">Bulk</option>
        </select>
      </label>
      <label>
        days(復元保持日数)
        <input
          v-model.number="transfer.days"
          type="number"
          name="days"
          min="1"
          max="365"
          :disabled="transfer.submitting"
        />
      </label>
      <label class="transfer-overwrite">
        <input
          v-model="transfer.overwrite"
          type="checkbox"
          name="overwrite"
          :disabled="transfer.submitting"
        />
        sha不一致の既存平文を上書きする
      </label>
      <button
        type="submit"
        class="btn-primary"
        :disabled="transfer.submitting || transfer.uuid.trim() === '' || transfer.out.trim() === ''"
      >取得を開始</button>
    </form>
  </section>

  <section class="card">
    <h2>job一覧</h2>
    <p v-if="jobs.lastError" class="message">
      job一覧の取得に失敗しました({{ jobs.lastError.code }})。
      空のdataは表示していません。
    </p>
    <p v-if="jobs.lastCancelError" class="message">
      jobの取り消しに失敗しました({{ jobs.lastCancelError.code }})。実行中のjobは
      完了まで実行されます(冪等なため再実行で回復できます)。
    </p>
    <template v-if="jobs.items.length > 0">
      <table class="pending-table">
        <thead>
          <tr>
            <th>種類</th>
            <th>対象</th>
            <th>状態</th>
            <th>進行</th>
            <th>現在path</th>
            <th>開始</th>
            <th>完了</th>
            <th>結果・error</th>
            <th>操作</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="job in jobRows" :key="job.id">
            <td>{{ job.kind }}</td>
            <td class="mono">{{ job.params?.uuid ?? '-' }}</td>
            <td>
              <span :class="`job-status st-${job.status}`">
                {{ statusLabel[job.status] ?? job.status }}
              </span>
            </td>
            <td class="progress-num">{{ progressText(job.progress) }}</td>
            <td class="mono job-current-path">{{ job.progress?.current_path ?? '' }}</td>
            <td class="mono">{{ job.created_at }}</td>
            <td class="mono">
              {{ ['succeeded', 'failed', 'interrupted', 'cancelled'].includes(job.status) ? job.updated_at : '' }}
            </td>
            <td>
              <template v-if="job.error">
                <span class="mono">{{ job.error.code }}</span>
                {{ job.error.message }}
              </template>
              <template v-else-if="job.result">
                <span class="mono">{{ job.result.dest }}</span>
                <span v-if="job.result.skip">(sha一致のためskip)</span>
              </template>
            </td>
            <td>
              <button
                v-if="cancellable(job.status)"
                type="button"
                class="btn-quiet"
                :aria-label="`job ${job.id} を取り消す`"
                @click="jobs.cancelJob(job.id)"
              >取り消す</button>
              <button
                v-if="rerunnable(job.status)"
                type="button"
                class="btn-quiet"
                :aria-label="`job ${job.id} を再実行`"
                @click="jobs.rerunGet(job)"
              >再実行</button>
            </td>
          </tr>
        </tbody>
      </table>
      <p class="muted">
        job一覧は自動更新します(設定タブのUI更新挙動で間隔を調整できます。
        待機中・実行中のjobが上位に表示されます)。
      </p>
    </template>
    <p v-else-if="jobs.lastError === null" class="muted">
      jobはまだありません(取得formから発行できます)。upload(push)はW5で追加予定です。
    </p>
  </section>
</template>

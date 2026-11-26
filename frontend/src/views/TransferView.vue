<script setup lang="ts">
import { computed, onMounted, onUnmounted, watch } from 'vue'
import { useRoute } from 'vue-router'
import TwoStepConfirm from '../components/TwoStepConfirm.vue'
import type { PushDryRunTarget } from '../api/client'
import {
  failedPushItems,
  jobTargetSummary,
  pushJobFiles,
  useJobsStore,
} from '../stores/jobs'
import { useTransferStore } from '../stores/transfer'

// TASK-17設計2.3・9章W4/W5(TASK-27/28): 転送タブ=取得form + upload form + job一覧 + polling。
// - 取得form(W4): uuid・版(省略=最新・version_id/pin指定)・出力dir・tier・days・
//   overwriteはtransfer storeが持ち、詳細画面の版行「取得」からの導線
//   (?get=<uuid>&version_id=...)をprefillする(job発行までは転送タブで確認=2.2)
// - upload form(W5): server側path列挙+title(?upload=<path> prefill) →
//   dry-run予測+接続先確認(実効profile・bucket・prefix・recipients指紋一覧) →
//   2段階確認(W3の部品)を経てupload jobを発行(POST /api/jobs/push)
// - job一覧: get/push両対応・進行N/M・現在path・対象ごとの結果(失敗対象行とerror code)・
//   再実行(冪等)・取消(queuedのみ)を進行中/完了行に置く(実行中の取消は409=完了まで
//   実行のため表示しない)。一覧はjobs store(GET /api/jobs。pollingは設計5.3・ui-refresh
//   の間隔既定10秒)
// - 実描画の観測は実browser(条件書frontend.tomlのexcluded参照。form値のprefill・発行・
//   取消・再実行の成否はtransfer/jobs storeの状態で担保)

const route = useRoute()
const jobs = useJobsStore()
const transfer = useTransferStore()

/** route queryのstring化(?get=&version_id=&upload=。重複時は先頭) */
const queryString = (key: string): string | undefined => {
  const value = route.query[key]
  if (typeof value === 'string') return value
  if (Array.isArray(value)) {
    const first = value.find((v): v is string => typeof v === 'string')
    return first
  }
  return undefined
}

/** route queryからget / upload formをprefill(1度だけ=手動入力を上書きしない) */
const prefill = (): void => {
  transfer.prefillFromQuery({
    get: queryString('get'),
    version_id: queryString('version_id'),
    upload: queryString('upload'),
  })
}

/** 実行中jobがある間のjob一覧自動更新(設計5.3: 転送タブ表示中はpolling) */
onMounted(() => {
  prefill()
  void jobs.fetchJobs()
  jobs.startPolling()
})

watch(
  () => route.query,
  () => {
    prefill()
  },
)

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

const fmtSize = (size: number | null): string => {
  if (size === null) return '-'
  if (size >= 1024 * 1024 * 1024) return `${(size / (1024 * 1024 * 1024)).toFixed(1)} GiB`
  if (size >= 1024 * 1024) return `${(size / (1024 * 1024)).toFixed(1)} MiB`
  if (size >= 1024) return `${(size / 1024).toFixed(1)} KiB`
  return `${size} B`
}

/** get formのsubmit(jobs storeのissueGetへ) */
const submitGet = (): void => {
  void transfer.submitGet()
}

/** upload formのdry-run実行(接続先・予測の確認。storeのrunDryRunへ) */
const runDryRun = (): void => {
  void transfer.runDryRun()
}

/** 2段階確認(TwoStepConfirm部品)の確定時に呼ばれる */
const submitPush = (): void => {
  void transfer.submitPush()
}

/** dry-run予測行の注意欄(対象解決失敗のerror・title未指定newのwarning・同一run内の
 *  先行newへの合流参照=uuid未発行のためmerge_target_indexで参照先を示す) */
const dryRunNote = (item: PushDryRunTarget, targets: PushDryRunTarget[]): string => {
  if (item.error) return `${item.error.code}: ${item.error.message}`
  if (item.warning) return item.warning
  if (item.merge_target_index !== undefined && item.merge_target_index !== null) {
    const preceding = targets[item.merge_target_index]
    return preceding ? `先行対象と同一内容のため合流します(${preceding.path})` : ''
  }
  return ''
}

/** 対象ごとの結果行の失敗表示(error code+対象path) */
const fileErrorText = (file: { error?: { code: string; message: string } }): string => {
  return file.error ? `${file.error.code}: ${file.error.message}` : ''
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
    <h2>upload(backup push)</h2>
    <p class="muted">
      server上の対象アーカイブ(.zip/.7z)を暗号化してS3へuploadします。まず予測と接続先
      (実効profile・bucket・prefix・recipients指紋)を確認してからuploadを開始してください。
    </p>
    <p v-if="transfer.uploadClientError" class="message">{{ transfer.uploadClientError }}</p>
    <p v-if="transfer.lastDryRunError" class="message">
      dry-run予測に失敗しました({{ transfer.lastDryRunError.code }}:
      {{ transfer.lastDryRunError.message }})。pathやtitleを確認してください。
    </p>
    <p v-if="transfer.lastUploadSubmitError" class="message">
      upload jobの発行に失敗しました({{ transfer.lastUploadSubmitError.code }})。
    </p>
    <p v-if="transfer.dryRunning" class="muted">dry-run予測を実行中...</p>
    <p v-if="transfer.uploadSubmitting" class="muted">upload jobを発行中...</p>

    <form class="transfer-upload-form" @submit.prevent="runDryRun">
      <div
        v-for="(row, idx) in transfer.uploadTargets"
        :key="idx"
        class="transfer-upload-row"
      >
        <label>
          対象path(server側path)
          <input
            :value="row.path"
            type="text"
            :name="`upload_path_${idx}`"
            :disabled="transfer.dryRunning || transfer.uploadSubmitting"
            @input="transfer.updateUploadTarget(idx, { path: ($event.target as HTMLInputElement).value })"
          />
        </label>
        <label>
          title(新規発行時は必須・既存合流/再PUTは省略可)
          <input
            :value="row.title"
            type="text"
            :name="`upload_title_${idx}`"
            :disabled="transfer.dryRunning || transfer.uploadSubmitting"
            @input="transfer.updateUploadTarget(idx, { title: ($event.target as HTMLInputElement).value })"
          />
        </label>
        <button
          type="button"
          class="btn-quiet"
          :disabled="transfer.dryRunning || transfer.uploadSubmitting"
          @click="transfer.removeUploadTarget(idx)"
        >削除</button>
      </div>

      <div class="scan-actions">
        <button
          type="button"
          class="btn-quiet"
          :disabled="transfer.dryRunning || transfer.uploadSubmitting"
          @click="transfer.addUploadTarget()"
        >対象行を追加</button>
        <button
          type="submit"
          class="btn-quiet"
          :disabled="transfer.dryRunning || transfer.uploadSubmitting"
        >予測・接続先を確認</button>
      </div>
    </form>

    <div v-if="transfer.dryRunResult" class="transfer-confirm-box">
      <h3>接続先確認</h3>
      <p>
        profile: <span class="mono">{{ transfer.dryRunResult.connection.profile }}</span> /
        bucket: <span class="mono">{{ transfer.dryRunResult.connection.bucket }}</span> /
        prefix: <span class="mono">{{ transfer.dryRunResult.connection.prefix }}</span>
      </p>
      <p class="muted">recipients({{ transfer.dryRunResult.connection.recipients.length }}):</p>
      <ul>
        <li
          v-for="fp in transfer.dryRunResult.connection.recipients"
          :key="fp"
          class="mono"
        >{{ fp }}</li>
      </ul>

      <h3>dry-run予測</h3>
      <p class="muted">
        計 {{ transfer.dryRunResult.summary.count }}件
        ({{ fmtSize(transfer.dryRunResult.summary.total_size) }}) /
        skip={{ transfer.dryRunResult.summary.skip }} /
        re_put={{ transfer.dryRunResult.summary.re_put }} /
        merge={{ transfer.dryRunResult.summary.merge }} /
        new={{ transfer.dryRunResult.summary.new }} /
        失敗={{ transfer.dryRunResult.summary.failed }}
      </p>
      <table class="pending-table">
        <thead>
          <tr>
            <th>予測</th>
            <th>path</th>
            <th>title</th>
            <th>size</th>
            <th>uuid / 注意</th>
          </tr>
        </thead>
        <tbody>
          <tr
            v-for="(item, idx) in transfer.dryRunResult.targets"
            :key="idx"
            :class="{ 'job-file-failed': item.action === 'error' }"
          >
            <td class="mono">{{ item.action }}</td>
            <td class="mono job-current-path">{{ item.path }}</td>
            <td>{{ item.title ?? '-' }}</td>
            <td class="mono">{{ fmtSize(item.size) }}</td>
            <td>
              <span v-if="item.uuid" class="mono">{{ item.uuid }}</span>
              <p
                v-if="dryRunNote(item, transfer.dryRunResult.targets)"
                :class="item.error ? 'message' : 'muted'"
              >{{ dryRunNote(item, transfer.dryRunResult.targets) }}</p>
            </td>
          </tr>
        </tbody>
      </table>

      <!-- 2段階確認(W3の部品と同一。「uploadを開始」→同位置が「確認してupload」へ置換。
           dialog不使用) -->
      <div v-if="transfer.isUploadConfirmed" class="scan-actions">
        <TwoStepConfirm
          label="uploadを開始"
          confirm-label="確認してupload"
          :disabled="transfer.uploadSubmitting"
          @confirm="submitPush"
        />
        <button
          type="button"
          class="btn-quiet"
          :disabled="transfer.uploadSubmitting"
          @click="transfer.cancelUploadConfirm()"
        >確認を取り消す</button>
      </div>
    </div>
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
          <template v-for="job in jobRows" :key="job.id">
            <tr>
              <td>{{ job.kind }}</td>
              <td class="mono job-current-path">{{ jobTargetSummary(job) }}</td>
              <td>
                <span :class="`job-status st-${job.status}`">
                  {{ statusLabel[job.status] ?? job.status }}
                </span>
              </td>
              <td class="progress-num">{{ progressText(job.progress) }}</td>
              <td class="mono job-current-path">{{ job.progress?.current_path ?? '' }}</td>
              <td class="mono">{{ job.created_at }}</td>
              <td class="mono">
                {{
                  ['succeeded', 'failed', 'interrupted', 'cancelled'].includes(job.status)
                    ? job.updated_at
                    : ''
                }}
              </td>
              <td>
                <template v-if="job.kind === 'get'">
                  <template v-if="job.error">
                    <span class="mono">{{ job.error.code }}</span>
                    {{ job.error.message }}
                  </template>
                  <template v-else-if="job.result">
                    <span class="mono">{{ job.result.dest }}</span>
                    <span v-if="job.result.skip">(sha一致のためskip)</span>
                  </template>
                </template>
                <template v-else-if="job.kind === 'push'">
                  <div v-if="job.error">
                    <span class="mono">{{ job.error.code }}</span>
                    {{ job.error.message }}
                  </div>
                  <!-- 結果列は集計(対象ごとの結果はjob行の下へinline展開) -->
                  <div v-if="job.result" class="mono">
                    ok={{ job.result.summary.ok }} skip={{ job.result.summary.skip }}
                    merge={{ job.result.summary.merge }} failed={{ job.result.summary.failed }}
                  </div>
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
                  @click="jobs.rerunJob(job)"
                >再実行</button>
              </td>
            </tr>
            <!-- push jobのper-file結果(job行の下へinline展開。<details>=JS不要方式。
                 失敗行のあるjobは展開した状態にする) -->
            <tr v-if="job.kind === 'push' && job.result" class="job-files-row">
              <td colspan="9">
                <details :open="failedPushItems(job).length > 0">
                  <summary>
                    対象ごとの結果 ({{ pushJobFiles(job).length }}件・失敗
                    {{ failedPushItems(job).length }}件)
                  </summary>
                  <table class="pending-table job-files-table">
                    <thead>
                      <tr>
                        <th>状態</th>
                        <th>path</th>
                        <th>処理</th>
                        <th>uuid</th>
                        <th>error</th>
                      </tr>
                    </thead>
                    <tbody>
                      <tr
                        v-for="(file, idx) in pushJobFiles(job)"
                        :key="idx"
                        :class="{ 'job-file-failed': file.status === 'failed' }"
                      >
                        <td class="mono">{{ file.status }}</td>
                        <td class="mono job-current-path">{{ file.path }}</td>
                        <td class="mono">{{ file.action ?? '-' }}</td>
                        <td class="mono">{{ file.uuid ?? '-' }}</td>
                        <td>{{ fileErrorText(file) }}</td>
                      </tr>
                    </tbody>
                  </table>
                </details>
              </td>
            </tr>
          </template>
        </tbody>
      </table>
      <p class="muted">
        job一覧は自動更新します(設定タブのUI更新挙動で間隔を調整できます。
        待機中・実行中のjobが上位に表示されます)。
      </p>
    </template>
    <p v-else-if="jobs.lastError === null" class="muted">
      jobはまだありません(取得form・upload formから発行できます)。
    </p>
  </section>
</template>

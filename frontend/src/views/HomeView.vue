<script setup lang="ts">
import { onMounted, onUnmounted } from 'vue'
import { useSyncStore } from '../stores/sync'
import { usePendingStore } from '../stores/pending'
import { useDiffStore } from '../stores/diff'
import { useRunsStore } from '../stores/runs'
import { useJobsStore } from '../stores/jobs'
import SyncStatus from '../components/SyncStatus.vue'

// TASK-17設計2.1(ホーム=状態の要約)・9章W1: sync状態・backup概况(前回backup)・
// 未対応サマリ(pending/diff)・実行中job区画を1画面で読ませる。各サマリ行は
// /backupsのfilter view(?filter=pending:<status>・?filter=diff)・/transferへの
// link(旧HomeViewの「後続phase」的link列は廃止し上記に置換)。次回backupの
// schedule表示・実行中jobの経過表示はW4〜W7で拡張。
const sync = useSyncStore()
const pending = usePendingStore()
const diff = useDiffStore()
const runs = useRunsStore()
const jobs = useJobsStore()

onMounted(() => {
  void sync.fetchHealth()
  // sync状態(保護API。P2〜P3はfail-closed 503のためlastErrorに閉鎖状態を保持する)
  void sync.fetchSyncState()
  // ホームサマリのdata源(失敗時は各storeのlastErrorへ保持し空dataは表示しない)
  void runs.fetchRuns()
  void pending.fetchPending()
  void diff.fetchDiff()
  void jobs.fetchJobs()
  // 実行中job区画の自動更新(設計5.3: ホーム表示中のjob追従。nav dot用polling
  // (App.vue)とは参照カウントで独立=ホームを離れてもnav dot分は継続)
  jobs.startPolling()
})

onUnmounted(() => {
  jobs.stopPolling()
})

/** pending statusの表示文言(PendingView由来の語彙) */
const pendingLabel: Record<string, string> = {
  unbacked: '未backup(unbacked)',
  changed: '変更あり(changed)',
  unknown: '確認不能(unknown)',
}
</script>

<template>
  <SyncStatus
    :health="sync.health"
    :message="sync.message"
    :sync-fetch="sync.syncFetch"
    :sync-status="sync.syncStatus"
    :sync-scope="sync.syncScope"
    :sync-fetched-at="sync.syncFetchedAt"
    :sync-entry-count="sync.syncEntryCount"
    :last-error="sync.lastError"
  />

  <section class="card">
    <h2>backup概况</h2>
    <p v-if="runs.lastError" class="message">
      run履歴の取得に失敗しました({{ runs.lastError.code }})。空のdataは表示していません。
    </p>
    <template v-else-if="runs.latestRun">
      <p>
        前回backup: <span class="mono">{{ runs.latestRun.date }}</span>(対象
        {{ runs.latestRun.entity_count }}実体・run
        <span class="mono">{{ runs.latestRun.batch_id }}</span
        >)
      </p>
      <p class="muted">
        runの一覧・各entityの版は<router-link to="/admin">管理</router-link>および
        <router-link to="/backups">バックアップ</router-link>で確認できます。
      </p>
    </template>
    <p v-else class="muted">
      前回backup: まだrun履歴がありません(backup実行後に表示されます)。
    </p>
    <p class="muted">次回backup: 手動実行のみ</p>
  </section>

  <section class="card">
    <h2>未対応・突合サマリ</h2>
    <h3>アップロード前カタログ(pending)</h3>
    <p v-if="pending.lastError" class="message">
      pending一覧の取得に失敗しました({{ pending.lastError.code }})。
      空のdataは表示していません。
    </p>
    <ul v-else>
      <li v-for="status in ['unbacked', 'changed', 'unknown']" :key="status">
        <router-link :to="`/backups?filter=pending:${status}`">
          <span :class="`st-${status}`">{{ pendingLabel[status] }}</span>
          {{ pending.counts[status] }}件
        </router-link>
      </li>
    </ul>

    <h3>完全性突合(diff)</h3>
    <p v-if="diff.diffView === 'error'" class="message">
      diff結果の取得に失敗しました({{ diff.lastError?.code }})。
      空のdataは表示していません。
    </p>
    <p v-else-if="diff.diffView === 'not_run'" class="muted">
      diffは未実行です(<router-link to="/backups?filter=diff">diff実行</router-link
      >はバックアップ一覧のdiff絞り込みから行えます)。
    </p>
    <ul v-else-if="diff.result">
      <li>
        <router-link to="/backups?filter=diff">
          missing(索引に在るがS3に無い) {{ diff.summary.missing }}件
        </router-link>
      </li>
      <li>
        <router-link to="/backups?filter=diff">
          unexpected(S3に在るが索引に無い) {{ diff.summary.unexpected }}件
        </router-link>
      </li>
      <li class="muted">実行時刻: {{ diff.result.performed_at }}</li>
    </ul>
  </section>

  <!-- 実行中job区画: 取得成功かつ実行中job無し(空配列)なら区画全体を非表示
  (設計2.1「空時は表示なし」)。完了済みjobは転送タブで確認するためここには
  出さない。取得失敗(lastError)のみ案内のために表示する -->
  <section v-if="jobs.lastError !== null || jobs.hasActiveJobs" class="card">
    <h2>実行中job</h2>
    <p v-if="jobs.lastError" class="message">
      job一覧の取得に失敗しました({{ jobs.lastError.code }})。
      空のdataは表示していません。
    </p>
    <template v-else>
      <table class="pending-table">
        <thead>
          <tr>
            <th>種類</th>
            <th>対象</th>
            <th>状態</th>
            <th>進行</th>
            <th>開始</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="job in jobs.activeJobs" :key="job.id">
            <td>{{ job.kind }}</td>
            <td class="mono">{{ job.params?.uuid ?? '-' }}</td>
            <td>
              <span :class="`job-status st-${job.status}`">{{ job.status }}</span>
            </td>
            <td class="progress-num">
              {{ job.progress === null ? '-' : `${job.progress.done}/${job.progress.total}` }}
            </td>
            <td class="mono">{{ job.created_at }}</td>
          </tr>
        </tbody>
      </table>
      <p>
        <router-link to="/transfer">転送画面で進行を確認</router-link>
      </p>
    </template>
  </section>
</template>

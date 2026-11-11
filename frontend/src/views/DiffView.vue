<script setup lang="ts">
import { onMounted } from 'vue'
import { useDiffStore } from '../stores/diff'

// TASK-9設計7章(frontend構成)・12章P6(diff UI)。diffのUI誘導状態はdiff storeの
// diffView getter・結果集計はsummary getterが持ち、diff実行はrunDiff action
// (POST /api/diffの応答をそのままresultへ保持)。実描画の観測は実browser
// (条件書frontend.tomlのexcluded参照)。
const diff = useDiffStore()

onMounted(() => {
  void diff.fetchDiff()
})
</script>

<template>
  <section class="card">
    <h2>完全性突合(diff)</h2>
    <p class="muted">
      local catalog cache(索引replica)とS3実態(list_versions)の全体突合結果です
      (CLIのdiffと同一語彙)。missing=索引に在るがS3に無いもの・unexpected=S3に
      在るが索引に無いもの。
    </p>

    <div class="scan-actions">
      <button type="button" class="btn-primary" :disabled="diff.running" @click="diff.runDiff()">
        {{ diff.running ? 'diff実行中...' : 'diff実行' }}
      </button>
      <span v-if="diff.diffView === 'result'" class="muted">
        実行時刻 {{ diff.result?.performed_at }}・entity {{ diff.summary.entries }}件・
        missing {{ diff.summary.missing }}件・unexpected {{ diff.summary.unexpected }}件
      </span>
    </div>

    <p v-if="diff.diffView === 'error'" class="message">
      取得に失敗しました({{ diff.lastError?.code }})。空のdataは表示していません。
    </p>
    <p v-else-if="diff.diffView === 'not_run'" class="muted">
      diffは未実行です(diff実行ボタンで突合を実行できます)。
    </p>

    <p v-if="diff.hasRunError" class="message">
      直前のdiff実行に失敗しました({{ diff.lastRunError?.code }})。
      <template v-if="diff.showingStaleResult">以下は保存済みの旧結果です。</template>
      <template v-else>保存済みの結果はありません(diff実行ボタンで再試行できます)。</template>
    </p>

    <template v-if="diff.diffView === 'result' && diff.result">
      <table v-if="diff.result.files.length > 0" class="pending-table">
        <thead>
          <tr>
            <th>uuid</th>
            <th>title</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="file in diff.result.files" :key="file.uuid">
            <td>{{ file.uuid }}</td>
            <td>{{ file.title }}</td>
          </tr>
        </tbody>
      </table>
      <h3>missing(索引に在るがS3に無い)</h3>
      <ul v-if="diff.result.missing.length > 0">
        <li v-for="key in diff.result.missing" :key="key">{{ key }}</li>
      </ul>
      <p v-else class="muted">0件</p>
      <h3>unexpected(S3に在るが索引に無い)</h3>
      <ul v-if="diff.result.unexpected.length > 0">
        <li v-for="key in diff.result.unexpected" :key="key">{{ key }}</li>
      </ul>
      <p v-else class="muted">0件</p>
    </template>
  </section>
</template>

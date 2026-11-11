<script setup lang="ts">
import { onMounted, ref } from 'vue'
import { useSyncStore } from '../stores/sync'
import { useEntitiesStore } from '../stores/entities'
import SyncStatus from '../components/SyncStatus.vue'

// TASK-9設計7章(EntitiesView=一覧・検索・絞込・sync状態+refresh)・4章D10(staleness
// 表示+手動refresh=POST /api/sync)・コードレビュー指摘高4。一覧・絞り込み状態は
// entities store(items/query/category/page+entitiesView getter)が持ち、手動refreshは
// sync storeのrefresh action(POST後にentities一覧を再取得)。実描画の観測は実browser
// (条件書frontend.tomlのexcluded参照)。

const sync = useSyncStore()
const entities = useEntitiesStore()

// 検索入力はlocal stateで持ち(文字入力のたびにfetchしない)適用時にstoreへ反映する
const queryInput = ref(entities.query)
// category絞込も同様に入力確定時に反映する(空文字列=絞り込み無し)
const categoryInput = ref(entities.category ?? '')

onMounted(() => {
  if (entities.entitiesView === 'not_fetched') {
    void entities.fetchEntities()
  }
})

// formのsubmitは検索語とcategory絞込を1回で反映する(1つのsubmit処理へ纏める=
// どちらか片方だけが適用される経路を作らない)
const applyFilters = (): void => {
  void entities.applyFilters(
    queryInput.value,
    categoryInput.value === '' ? null : categoryInput.value,
  )
}

const clearFilters = (): void => {
  queryInput.value = ''
  categoryInput.value = ''
  void entities.applyFilters('', null)
}

const gotoPage = (page: number): void => {
  if (page < 1 || page > entities.totalPages || page === entities.page) return
  entities.setPage(page)
  void entities.fetchEntities()
}

const fmtSize = (size: number): string => {
  if (size >= 1024 * 1024 * 1024) return `${(size / (1024 * 1024 * 1024)).toFixed(1)} GB`
  if (size >= 1024 * 1024) return `${(size / (1024 * 1024)).toFixed(1)} MB`
  if (size >= 1024) return `${(size / 1024).toFixed(1)} KB`
  return `${size} B`
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
    <h2>entity一覧</h2>
    <div class="scan-actions">
      <button type="button" class="btn-primary" :disabled="sync.refreshing" @click="sync.refresh()">
        {{ sync.refreshing ? 'sync実行中...' : 'sync手動refresh' }}
      </button>
      <span class="muted">remote索引を再取得して一覧を更新します(POST /api/sync)</span>
    </div>
    <p v-if="sync.lastRefreshError" class="message">
      直前の手動refreshに失敗しました({{ sync.lastRefreshError.code }})。表示中の一覧は
      最後に取得できた内容です。
    </p>

    <form class="entity-filters" @submit.prevent="applyFilters">
      <label>
        検索(title/uuid/所在pathの部分一致)
        <input v-model="queryInput" type="search" name="query" />
      </label>
      <label>
        category絞込(locationのcategory一致・空で無し)
        <input v-model="categoryInput" type="search" name="category" />
      </label>
      <button type="submit" class="btn-primary">適用</button>
      <button type="button" class="btn-quiet" @click="clearFilters">解除</button>
    </form>

    <p v-if="entities.entitiesView === 'loading'" class="muted">読み込み中...</p>
    <p v-else-if="entities.entitiesView === 'error'" class="message">
      一覧の取得に失敗しました({{ entities.lastError?.code }})。空のdataは表示していません。
    </p>
    <p v-else-if="entities.entitiesView === 'not_fetched'" class="muted">未取得です。</p>

    <table v-if="entities.entitiesView === 'result' && entities.items.length > 0" class="pending-table">
      <thead>
        <tr>
          <th>title</th>
          <th>uuid</th>
          <th>所在</th>
          <th>版数</th>
          <th>latest</th>
          <th>size</th>
          <th>pins</th>
        </tr>
      </thead>
      <tbody>
        <tr v-for="item in entities.items" :key="item.uuid">
          <td>
            <router-link :to="`/entities/${item.uuid}`">{{ item.title }}</router-link>
          </td>
          <td class="mono">{{ item.uuid }}</td>
          <td>{{ item.locations.length }}</td>
          <td>{{ item.versions }}</td>
          <td class="mono">{{ item.latest_date }}</td>
          <td class="mono">{{ fmtSize(item.latest_size) }}</td>
          <td>{{ item.pins.length }}</td>
        </tr>
      </tbody>
    </table>
    <p v-else-if="entities.entitiesView === 'result'" class="muted">
      0件(検索語・category絞り込みを確認するか、sync手動refreshで索引を更新してください)。
    </p>

    <nav v-if="entities.entitiesView === 'result' && entities.totalPages > 1" class="entity-pager">
      <button type="button" class="btn-quiet" :disabled="entities.page <= 1" @click="gotoPage(entities.page - 1)">
        前へ
      </button>
      <span class="muted">{{ entities.page }} / {{ entities.totalPages }} page</span>
      <button
        type="button"
        class="btn-quiet"
        :disabled="entities.page >= entities.totalPages"
        @click="gotoPage(entities.page + 1)"
      >
        次へ
      </button>
    </nav>
  </section>
</template>

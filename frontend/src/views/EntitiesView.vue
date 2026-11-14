<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'
import { useRoute } from 'vue-router'
import { useSyncStore } from '../stores/sync'
import { useEntitiesStore } from '../stores/entities'
import { usePendingStore } from '../stores/pending'
import { useDiffStore } from '../stores/diff'
import SyncStatus from '../components/SyncStatus.vue'

// TASK-17設計2.2・9章W1: バックアップタブ(/backups)。現行EntitiesView(一覧・
// 検索・絞込・sync状態+refresh)に、旧PendingView・DiffViewをquery paramのfilter
// viewへ統合(?filter=pending:<status>=pending一覧への切替+scan再実行button・
// ?filter=diff=diff結果(missing・unexpected両一覧)の表示。旧path(/pending・/diff)
// はrouteを持たない)。filterの解析はroute.query由来・一覧・絞り込み状態は各store
// が持つ。実描画の観測は実browser(条件書frontend.tomlのexcluded参照)。

const route = useRoute()
const sync = useSyncStore()
const entities = useEntitiesStore()
const pending = usePendingStore()
const diff = useDiffStore()

/** pending status語彙(server応答の4状態。filterはうち3statusへの導線+ok含む) */
type PendingStatus = 'unbacked' | 'changed' | 'ok' | 'unknown'

const isPendingStatus = (v: string): v is PendingStatus =>
  v === 'unbacked' || v === 'changed' || v === 'ok' || v === 'unknown'

/** route.query.filterの正規化(配列時は先頭・非文字列は無効) */
const filterParam = computed<string | null>(() => {
  const raw = route.query.filter
  const value = Array.isArray(raw) ? raw[0] : raw
  return typeof value === 'string' && value !== '' ? value : null
})

/** ?filter=pending:<status>のstatus(不一致はnull=filter viewでない) */
const pendingFilter = computed<PendingStatus | null>(() => {
  if (filterParam.value === null) return null
  const status = filterParam.value.replace(/^pending:/, '')
  return filterParam.value.startsWith('pending:') && isPendingStatus(status) ? status : null
})

/** ?filter=diff */
const diffFilter = computed<boolean>(() => filterParam.value === 'diff')

/** 表示するfilter viewの種別 */
const filterView = computed<'pending' | 'diff' | null>(() => {
  if (pendingFilter.value !== null) return 'pending'
  if (diffFilter.value) return 'diff'
  return null
})

/** filter viewのdata源の読み込み(初回mountとquery変更で呼ぶ) */
const loadForFilter = (): void => {
  if (pendingFilter.value !== null) {
    void pending.fetchPending()
  } else if (diffFilter.value) {
    void diff.fetchDiff()
  } else if (entities.entitiesView === 'not_fetched') {
    void entities.fetchEntities()
  }
}

onMounted(loadForFilter)

// 同一componentでのquery変更(ホームのサマリ行からの遷移・一覧へ戻る)にも追従する
watch(filterView, () => {
  loadForFilter()
})

// pending filter viewの表示行(当該statusのみ)
const pendingItems = computed(() =>
  pendingFilter.value === null
    ? []
    : pending.items.filter((item) => item.status === pendingFilter.value),
)

// missing object keyから対象entityのuuid一覧(key構造は`<prefix>data/{uuid}/…`の
// uuid基準=layout条件。versions/pinsのobject keyはすべてこの構造のため、`data/`
// 直下の階層=対象entityのuuid。抽出結果はfilesのuuidとの照合で使うため、構造に
// 合わないkeyは一覧へ現れない)
const missingEntityUuids = computed<Set<string>>(() => {
  const ids = new Set<string>()
  for (const key of diff.result?.missing ?? []) {
    const at = key.indexOf('/data/')
    if (at === -1) continue
    const rest = key.slice(at + '/data/'.length)
    const uuid = rest.split('/', 1)[0]
    if (uuid !== '') ids.add(uuid)
  }
  return ids
})

// diff filter viewのentity一覧(設計2.2: missing対象entityのみ。応答filesは索引の
// 全entity一覧のため、missing対象uuidとの照合で絞る=missing 0件なら正常entityは
// 並ばない)
const missingEntityFiles = computed(() =>
  diff.result === null
    ? []
    : diff.result.files.filter((file) => missingEntityUuids.value.has(file.uuid)),
)

// 検索入力はlocal stateで持ち(文字入力のたびにfetchしない)適用時にstoreへ反映する
const queryInput = ref(entities.query)
// category絞込も同様に入力確定時に反映する(空文字列=絞り込み無し)
const categoryInput = ref(entities.category ?? '')

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

// TASK-16設計3章(pending status列): status -> 色分けclass(visual design。
// st-* class selector単層でCSS側は解釈=構造位置selectorに依存しない)
const statusClass = (status: string): string =>
  status === 'unbacked' || status === 'changed' || status === 'ok'
    ? `st-${status}`
    : 'st-unknown'

/** pending statusの表示文言 */
const pendingLabel: Record<string, string> = {
  unbacked: '未backup(unbacked)',
  changed: '変更あり(changed)',
  ok: 'backup済み(ok)',
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

  <!-- ?filter=pending:<status> = pending一覧への切替(scan再実行button付き) -->
  <section v-if="filterView === 'pending' && pendingFilter !== null" class="card">
    <h2>アップロード前カタログ({{ pendingLabel[pendingFilter] }})</h2>
    <p class="muted">
      ホームの未対応サマリからの絞り込み表示です({{ pendingFilter }}のみ)。scan対象は
      pipeline.toml [catalog.scan]の登録root配下(local FS走査・symlinkは辿りません)。
    </p>
    <div class="scan-actions">
      <button type="button" class="btn-primary" :disabled="pending.scanning" @click="pending.runScan()">
        {{ pending.scanning ? 'scan実行中...' : 'scan再実行' }}
      </button>
      <router-link to="/backups" class="btn-quiet">一覧へ戻る</router-link>
    </div>

    <p v-if="pending.lastError" class="message">
      取得に失敗しました({{ pending.lastError.code }})。空のdataは表示していません。
    </p>
    <p v-if="pending.lastScanError" class="message">
      直前のscan実行に失敗しました({{ pending.lastScanError.code }})。
      以下は最後に取得できた一覧です。
    </p>

    <table v-if="pendingItems.length > 0" class="pending-table">
      <thead>
        <tr>
          <th>status</th>
          <th>path</th>
          <th>size</th>
          <th>mtime</th>
        </tr>
      </thead>
      <tbody>
        <tr v-for="item in pendingItems" :key="`${item.source_root}:${item.path}`">
          <td :class="statusClass(item.status)">{{ item.status }}</td>
          <td class="mono">{{ item.path }}</td>
          <td class="mono">{{ fmtSize(item.size) }}</td>
          <td class="mono">{{ item.mtime }}</td>
        </tr>
      </tbody>
    </table>
    <p v-else-if="!pending.lastError" class="muted">
      対象0件({{ pendingFilter }}の対象はありません)。
    </p>
  </section>

  <!-- ?filter=diff = diff結果(missing一覧+unexpected一覧の両方=旧DiffView相当) -->
  <section v-else-if="filterView === 'diff'" class="card">
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
      <router-link to="/backups" class="btn-quiet">一覧へ戻る</router-link>
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
      <h3>missing対象entity</h3>
      <table v-if="missingEntityFiles.length > 0" class="pending-table">
        <thead>
          <tr>
            <th>uuid</th>
            <th>title</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="file in missingEntityFiles" :key="file.uuid">
            <td class="mono">{{ file.uuid }}</td>
            <td>
              <router-link :to="`/backups/${file.uuid}`">{{ file.title }}</router-link>
            </td>
          </tr>
        </tbody>
      </table>
      <p v-else class="muted">0件(missing対象のentityはありません)</p>
      <h3>missing(索引に在るがS3に無い)</h3>
      <ul v-if="diff.result.missing.length > 0">
        <li v-for="key in diff.result.missing" :key="key" class="mono">{{ key }}</li>
      </ul>
      <p v-else class="muted">0件</p>
      <h3>unexpected(S3に在るが索引に無い)</h3>
      <ul v-if="diff.result.unexpected.length > 0">
        <li v-for="key in diff.result.unexpected" :key="key" class="mono">{{ key }}</li>
      </ul>
      <p v-else class="muted">0件</p>
    </template>
  </section>

  <!-- 通常(一覧・検索・絞込) -->
  <section v-else class="card">
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
            <router-link :to="`/backups/${item.uuid}`">{{ item.title }}</router-link>
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

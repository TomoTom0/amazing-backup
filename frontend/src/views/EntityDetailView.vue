<script setup lang="ts">
import { computed, onMounted } from 'vue'
import { useRoute } from 'vue-router'
import { useEntitiesStore } from '../stores/entities'

// TASK-9設計7章(EntityDetailView)・6章(:uuid=version_list付き詳細・
// :uuid/versions=state付き版一覧)・コードレビュー指摘高4。詳細・versionsの状態は
// entities store(detail/versions/detailError/versionsError)が持つ(詳細とversionsで
// error状態を分離し並行取得で片方の成功がもう片方の失敗を隠さない)。実描画の観測は
// 実browser(条件書frontend.tomlのexcluded参照)。

const route = useRoute()
const entities = useEntitiesStore()

// vue-routerのparamsはstring | string[]のため型ガードで確定する
const uuidFromRoute = computed<string | null>(() => {
  const uuid: unknown = route.params.uuid
  if (typeof uuid === 'string' && uuid !== '') return uuid
  return null
})

onMounted(() => {
  const uuid = uuidFromRoute.value
  if (uuid === null) return
  if (entities.selectedUuid !== uuid || entities.detail === null) {
    void entities.fetchEntityDetail(uuid)
  }
  void entities.fetchEntityVersions(uuid)
})

const fmtSize = (size: number): string => {
  if (size >= 1024 * 1024 * 1024) return `${(size / (1024 * 1024 * 1024)).toFixed(1)} GB`
  if (size >= 1024 * 1024) return `${(size / (1024 * 1024)).toFixed(1)} MB`
  if (size >= 1024) return `${(size / 1024).toFixed(1)} KB`
  return `${size} B`
}
</script>

<template>
  <section class="card">
    <h2>entity詳細</h2>
    <p>
      <router-link to="/entities">entity一覧へ戻る</router-link>
    </p>

    <p v-if="entities.detailError" class="message">
      詳細の取得に失敗しました({{ entities.detailError.code }})。対象のentityが見つからない
      場合は一覧から選び直してください。
    </p>
    <p v-if="entities.versionsError" class="message">
      版一覧(state付き)の取得に失敗しました({{ entities.versionsError.code }})。
    </p>

    <template v-if="entities.detail">
      <h3>{{ entities.detail.title }}</h3>
      <p class="muted">uuid: {{ entities.detail.uuid }}</p>
      <p class="muted">
        版数: {{ entities.detail.versions }}・latest: {{ entities.detail.latest_date }}
        ({{ fmtSize(entities.detail.latest_size) }})
      </p>

      <h4>所在(locations)</h4>
      <table v-if="entities.detail.locations.length > 0" class="pending-table">
        <thead>
          <tr>
            <th>source_path</th>
            <th>category</th>
            <th>label</th>
            <th>last_backup</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="loc in entities.detail.locations" :key="loc.source_path">
            <td>{{ loc.source_path }}</td>
            <td>{{ loc.category }}</td>
            <td>{{ loc.label }}</td>
            <td>{{ loc.last_backup }}</td>
          </tr>
        </tbody>
      </table>
      <p v-else class="muted">所在は登録されていません。</p>

      <h4>pins</h4>
      <table v-if="entities.detail.pins.length > 0" class="pending-table">
        <thead>
          <tr>
            <th>pin</th>
            <th>date</th>
            <th>size</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="pin in entities.detail.pins" :key="pin.pin">
            <td>{{ pin.pin }}</td>
            <td>{{ pin.date }}</td>
            <td>{{ fmtSize(pin.source_size) }}</td>
          </tr>
        </tbody>
      </table>
      <p v-else class="muted">pinはありません。</p>

      <h4>版一覧(version_list)</h4>
      <table v-if="entities.detail.version_list.length > 0" class="pending-table">
        <thead>
          <tr>
            <th>date</th>
            <th>version_ts</th>
            <th>version_id</th>
            <th>batch_id</th>
            <th>size</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="v in entities.detail.version_list" :key="v.version_id">
            <td>{{ v.date }}</td>
            <td>{{ v.version_ts }}</td>
            <td>{{ v.version_id }}</td>
            <td>{{ v.batch_id }}</td>
            <td>{{ fmtSize(v.source_size) }}</td>
          </tr>
        </tbody>
      </table>
      <p v-else class="muted">版はありません。</p>
    </template>
    <p v-else-if="!entities.detailError" class="muted">詳細を読み込み中...</p>

    <template v-if="entities.versions">
      <h4>版の現行/noncurrent(state付き一覧)</h4>
      <table v-if="entities.versions.versions.length > 0" class="pending-table">
        <thead>
          <tr>
            <th>date</th>
            <th>version_ts</th>
            <th>version_id</th>
            <th>size</th>
            <th>state</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="v in entities.versions.versions" :key="v.version_id">
            <td>{{ v.date }}</td>
            <td>{{ v.version_ts }}</td>
            <td>{{ v.version_id }}</td>
            <td>{{ fmtSize(v.source_size) }}</td>
            <td>{{ v.state }}</td>
          </tr>
        </tbody>
      </table>
      <p v-else class="muted">版はありません。</p>
    </template>
  </section>
</template>

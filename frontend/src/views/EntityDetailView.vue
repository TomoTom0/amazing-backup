<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'
import { useRoute } from 'vue-router'
import { useEntitiesStore } from '../stores/entities'

// TASK-9設計7章(EntityDetailView)・6章(:uuid=version_list付き詳細・
// :uuid/versions=state付き版一覧)・コードレビュー指摘高4。詳細・versionsの状態は
// entities store(detail/versions/detailError/versionsError)が持つ(詳細とversionsで
// error状態を分離し並行取得で片方の成功がもう片方の失敗を隠さない)。
// W2(TASK-17設計2.2): tag編集(追加form+chipのx削除=PUT /api/entities/:uuid/tagsで
// 一括置換)と所属collection表示。実描画の観測は実browser(条件書frontend.tomlの
// excluded参照。tag編集の成否はstoreのlastTagEditError/detail.tagsで担保)。
// W2レビュー指摘高1対応(TASK-19): 保存中(tagEditSaving)はtag追加form・chipのxを
// 無効化する(保存応答待ち中に同じ旧配列から作った後続PUTが先行編集を消すのを防ぐ)。

const route = useRoute()
const entities = useEntitiesStore()

/** tag追加formの入力(local state。文字入力のたびにPUTしない) */
const tagInput = ref('')

/** tag追加(formのsubmitでPUT。重複tagは無視=冪等な置換と同一結果にしておく) */
const addTag = (): void => {
  const uuid = uuidFromRoute.value
  const detail = entities.detail
  if (uuid === null || detail === null) return
  const tag = tagInput.value.trim()
  tagInput.value = ''
  if (tag === '' || detail.tags.includes(tag)) return
  void entities.updateTags(uuid, [...detail.tags, tag])
}

/** tag削除(chipのx。PUT一括置換で当該tagを除いた一覧へ) */
const removeTag = (tag: string): void => {
  const uuid = uuidFromRoute.value
  const detail = entities.detail
  if (uuid === null || detail === null) return
  void entities.updateTags(uuid, detail.tags.filter((t) => t !== tag))
}

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

// sync後の一覧再取得(refetchEntities)で詳細・versionsが失効(detail=null化)した
// 場合、表示中の画面でも再取得する(mount時のみでは失効後に「読み込み中」で
// 止まる)。失効前に開始した旧応答はstore側(detailEpoch)で不採用になるため
// ここでの再取得結果が採用される
watch(
  () => entities.detailEpoch,
  () => {
    const uuid = uuidFromRoute.value
    if (uuid === null) return
    void entities.fetchEntityDetail(uuid)
    void entities.fetchEntityVersions(uuid)
  },
)

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
      <router-link to="/backups">entity一覧へ戻る</router-link>
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
            <td>{{ loc.label === '' ? '(ラベルなし)' : loc.label }}</td>
            <td>{{ loc.last_backup }}</td>
          </tr>
        </tbody>
      </table>
      <p v-else class="muted">所在は登録されていません。</p>

      <h4>tag</h4>
      <p v-if="entities.lastTagEditError" class="message">
        tag編集に失敗しました({{ entities.lastTagEditError.code }})。tag名は小文字英数字と-
        からなる64文字以内で指定してください。
      </p>
      <p v-if="entities.tagEditSaving" class="muted">tag保存中...</p>
      <div v-if="entities.detail.tags.length > 0">
        <span v-for="tag in entities.detail.tags" :key="tag" class="tag-chip">
          {{ tag }}
          <button
            type="button"
            class="tag-chip-x"
            :aria-label="`tag ${tag} を削除`"
            :disabled="entities.tagEditSaving"
            @click="removeTag(tag)"
          >x</button>
        </span>
      </div>
      <p v-else class="muted">tagは付けられていません。</p>
      <form class="tag-add-form" @submit.prevent="addTag">
        <label>
          tag追加(小文字英数字と-)
          <input v-model="tagInput" type="text" name="tag" :disabled="entities.tagEditSaving" />
        </label>
        <button type="submit" class="btn-primary" :disabled="entities.tagEditSaving">tag追加</button>
      </form>

      <h4>所属collection</h4>
      <div v-if="entities.detail.collections.length > 0">
        <span v-for="name in entities.detail.collections" :key="name" class="tag-chip">
          {{ name }}
        </span>
      </div>
      <p v-else class="muted">所属するcollectionはありません。</p>

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
            <td class="mono">{{ v.version_ts }}</td>
            <td class="mono">{{ v.version_id }}</td>
            <td class="mono">{{ v.batch_id }}</td>
            <td class="mono">{{ fmtSize(v.source_size) }}</td>
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
            <td class="mono">{{ v.version_ts }}</td>
            <td class="mono">{{ v.version_id }}</td>
            <td class="mono">{{ fmtSize(v.source_size) }}</td>
            <td>{{ v.state }}</td>
          </tr>
        </tbody>
      </table>
      <p v-else class="muted">版はありません。</p>
    </template>
  </section>
</template>

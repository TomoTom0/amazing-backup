<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'
import { useRoute } from 'vue-router'
import { useEntitiesStore } from '../stores/entities'
import type { EntityLocation } from '../api/client'
import TwoStepConfirm from '../components/TwoStepConfirm.vue'

// TASK-9設計7章(EntityDetailView)・6章(:uuid=version_list付き詳細・
// :uuid/versions=state付き版一覧)・コードレビュー指摘高4。詳細・versionsの状態は
// entities store(detail/versions/detailError/versionsError)が持つ(詳細とversionsで
// error状態を分離し並行取得で片方の成功がもう片方の失敗を隠さない)。
// W2(TASK-17設計2.2): tag編集(追加form+chipのx削除=PUT /api/entities/:uuid/tagsで
// 一括置換)と所属collection表示。実描画の観測は実browser(条件書frontend.tomlの
// excluded参照。tag編集の成否はstoreのlastTagEditError/detail.tagsで担保)。
// W2レビュー指摘高1対応(TASK-19): 保存中(tagEditSaving)はtag追加form・chipのxを
// 無効化する(保存応答待ち中に同じ旧配列から作った後続PUTが先行編集を消すのを防ぐ)。
// W3(TASK-22・設計2.2/8章): 文脈操作の名前変更(title編集form→POST rename)と
// 所在行ごとの分類変更(category/label編集form→POST location)。実行は2段階確認
// 部品(TwoStepConfirm=「変更」→同位置が「確認して実行」。dialog不使用)を経る。
// 成否はstoreのlastRenameError/lastLocationError・detail.title/locationsで担保。

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

/** 名前変更formの入力(local state) */
const titleInput = ref('')

/** 名前変更(2段階確認の確定から。空titleは送信しない) */
const submitRename = (): void => {
  const uuid = uuidFromRoute.value
  if (uuid === null) return
  const title = titleInput.value.trim()
  if (title === '') return
  titleInput.value = ''
  void entities.renameEntity(uuid, title)
}

/** 所在編集中の行(source_path。null=編集中の行なし) */
const editingLocation = ref<string | null>(null)
/** 所在編集formの入力(編集開始時に対象行の現値で初期化) */
const locCategoryInput = ref('')
const locLabelInput = ref('')

/** 所在行の編集開始(現値で初期化し直前の失敗表示を消す) */
const startLocationEdit = (loc: EntityLocation): void => {
  editingLocation.value = loc.source_path
  locCategoryInput.value = loc.category
  locLabelInput.value = loc.label
}

/** 所在編集の取消 */
const cancelLocationEdit = (): void => {
  editingLocation.value = null
}

/** 所在の分類変更(2段階確認の確定から。空categoryは送信しない) */
const submitLocation = (sourcePath: string): void => {
  const uuid = uuidFromRoute.value
  if (uuid === null) return
  const category = locCategoryInput.value.trim()
  const label = locLabelInput.value.trim()
  if (category === '') return
  editingLocation.value = null
  void entities.updateLocation(uuid, sourcePath, category, label)
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
      <p v-if="entities.lastLocationError" class="message">
        所在の分類変更に失敗しました({{ entities.lastLocationError.code }})。
        categoryは小文字英数字と-の64文字以内・labelは先頭/や../を含めません
        (対象行はsource_path完全一致で指定されます)。
      </p>
      <p v-if="entities.locationSaving" class="muted">分類変更を実行中...</p>
      <table v-if="entities.detail.locations.length > 0" class="pending-table">
        <thead>
          <tr>
            <th>source_path</th>
            <th>category</th>
            <th>label</th>
            <th>last_backup</th>
            <th>操作</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="loc in entities.detail.locations" :key="loc.source_path">
            <td>{{ loc.source_path }}</td>
            <td v-if="editingLocation === loc.source_path">
              <label>
                category
                <input
                  v-model="locCategoryInput"
                  type="text"
                  name="category"
                  :disabled="entities.locationSaving"
                />
              </label>
            </td>
            <td v-else>{{ loc.category }}</td>
            <td v-if="editingLocation === loc.source_path">
              <label>
                label(空=ラベルなし)
                <input
                  v-model="locLabelInput"
                  type="text"
                  name="label"
                  :disabled="entities.locationSaving"
                />
              </label>
            </td>
            <td v-else>{{ loc.label === '' ? '(ラベルなし)' : loc.label }}</td>
            <td>{{ loc.last_backup }}</td>
            <td>
              <template v-if="editingLocation === loc.source_path">
                <TwoStepConfirm
                  label="変更"
                  :disabled="entities.locationSaving || locCategoryInput.trim() === ''"
                  @confirm="submitLocation(loc.source_path)"
                />
                <button
                  type="button"
                  class="btn-quiet"
                  :disabled="entities.locationSaving"
                  @click="cancelLocationEdit"
                >取消</button>
              </template>
              <button
                v-else
                type="button"
                class="btn-quiet"
                :disabled="entities.locationSaving"
                @click="startLocationEdit(loc)"
              >編集</button>
            </td>
          </tr>
        </tbody>
      </table>
      <p v-else class="muted">所在は登録されていません。</p>

      <h4>名前変更</h4>
      <p v-if="entities.lastRenameError" class="message">
        名前変更に失敗しました({{ entities.lastRenameError.code }})。titleは64文字以内で
        /・改行・制御文字を含めません。
      </p>
      <p v-if="entities.lastRenameWarning" class="message">
        {{ entities.lastRenameWarning }}
      </p>
      <p v-if="entities.renameSaving" class="muted">名前変更を実行中...</p>
      <div class="inline-edit-form">
        <label>
          新しいtitle
          <input
            v-model="titleInput"
            type="text"
            name="title"
            :disabled="entities.renameSaving"
          />
        </label>
        <TwoStepConfirm
          label="名前変更"
          :disabled="entities.renameSaving || titleInput.trim() === ''"
          @confirm="submitRename"
        />
      </div>

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

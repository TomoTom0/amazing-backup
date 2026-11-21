<script setup lang="ts">
import type { FolderNode } from '../stores/folders'

// TASK-17設計8章(folder tree部品)・2.2(folder構造ビュー=左にtree・右に選択folder
// 配下のentity一覧): ul入れ子・13px・展開はdetails open(JS不要)・選択folderは
// --fg+下線(router-link-activeと同一表現はstyle.cssの.folder-tree a.is-selected)。
// 実描画は実browser観測のため条件書frontend.tomlのexcluded参照(data基盤は
// stores/folders.tsのtree getterで担保)。

defineProps<{ nodes: FolderNode[]; selectedPath: string | null }>()

const emit = defineEmits<{ (e: 'select', path: string): void }>()

const select = (path: string): void => {
  emit('select', path)
}
</script>

<template>
  <ul>
    <li v-for="node in nodes" :key="node.path">
      <details v-if="node.children.length > 0" open>
        <summary>
          <a
            href="#"
            :class="{ 'is-selected': selectedPath === node.path }"
            @click.prevent="select(node.path)"
          >{{ node.label }}</a>
          <span class="folder-count">{{ node.entity_count }}</span>
        </summary>
        <FolderTreeNode :nodes="node.children" :selected-path="selectedPath" @select="select" />
      </details>
      <template v-else>
        <a
          href="#"
          :class="{ 'is-selected': selectedPath === node.path }"
          @click.prevent="select(node.path)"
        >{{ node.label }}</a>
        <span class="folder-count">{{ node.entity_count }}</span>
      </template>
    </li>
  </ul>
</template>

import { defineStore } from 'pinia'

/** UI更新挙動(自動更新間隔)設定のlocalStorage key(設計2.4: frontend側保存・
 *  server設定には含めない) */
const STORAGE_KEY = 'amazing-backup.ui-refresh-interval'

/** 既定の自動更新間隔(10秒=設計2.4) */
const DEFAULT_INTERVAL_MS = 10000

/** 保存値の10進文字列判定(「数値は10進文字列・無効は"null"」のみ有効とする) */
const DECIMAL_RE = /^\d+$/

/**
 * UI更新挙動(自動更新間隔)設定store(TASK-17設計2.4・5.3・9章W1)。間隔は
 * localStorageのみで保存・読込しserver側設定へは含めない(GET/PUT /api/settingsは
 * W7実装。load/setIntervalMsともfetchを呼ばない)。無効(null)も保存できる。
 * 不正な保存値(非数値文字列)は既定10000msへ戻す。W1では保持のみ(使用側の
 * polling実行はW4の転送タブ実装)。
 */
export const useUiRefreshStore = defineStore('ui-refresh', {
  state: () => ({
    /** 自動更新間隔ms(null=自動更新無効) */
    intervalMs: DEFAULT_INTERVAL_MS as number | null,
  }),
  actions: {
    /** localStorageの保存値の読込(未保存・非数値は既定10000ms・"null"は無効) */
    load(): void {
      const saved = localStorage.getItem(STORAGE_KEY)
      if (saved === null) {
        this.intervalMs = DEFAULT_INTERVAL_MS
        return
      }
      if (saved === 'null') {
        this.intervalMs = null
        return
      }
      this.intervalMs = DECIMAL_RE.test(saved) ? Number(saved) : DEFAULT_INTERVAL_MS
    },
    /** 間隔の設定(stateとlocalStorageの両方へ反映。null=無効) */
    setIntervalMs(intervalMs: number | null): void {
      this.intervalMs = intervalMs
      localStorage.setItem(STORAGE_KEY, String(intervalMs))
    },
  },
})

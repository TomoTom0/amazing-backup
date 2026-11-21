import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createPinia, setActivePinia } from 'pinia'
import { useUiRefreshStore } from './ui-refresh'

// UI更新挙動(自動更新間隔)設定のfrontend側保存(TASK-17設計2.4・5.3・9章W1):
// 既定10秒・無効可・保存はlocalStorageのみでserver設定でない。localStorageは
// browser APIのためvitest(node環境)ではvi.stubGlobalでStorage相当を注入して観測
// する(seam)。実装前のためsrc/stores/ui-refresh.tsは未実装=本fileのtestは実行
// failを許容(実装後に緑化)。

/** localStorage相当のin-memory実装(vi.stubGlobal注入用の観測seam) */
class MemoryStorage implements Storage {
  private readonly map = new Map<string, string>()
  get length(): number {
    return this.map.size
  }
  key(index: number): string | null {
    return Array.from(this.map.keys())[index] ?? null
  }
  getItem(key: string): string | null {
    return this.map.get(key) ?? null
  }
  setItem(key: string, value: string): void {
    this.map.set(key, value)
  }
  removeItem(key: string): void {
    this.map.delete(key)
  }
  clear(): void {
    this.map.clear()
  }
}

const STORAGE_KEY = 'amazing-backup.ui-refresh-interval'

let storage: MemoryStorage

const stubFetchCounter = (): string[] => {
  const calls: string[] = []
  vi.stubGlobal(
    'fetch',
    vi.fn().mockImplementation((input: RequestInfo | URL, init?: RequestInit) => {
      const url = typeof input === 'string' ? input : input.toString()
      calls.push(`${init?.method ?? 'GET'} ${url}`)
      return Promise.resolve(new Response('{}', { status: 200 }))
    }),
  )
  return calls
}

beforeEach(() => {
  setActivePinia(createPinia())
  storage = new MemoryStorage()
  vi.stubGlobal('localStorage', storage)
})

afterEach(() => {
  vi.unstubAllGlobals()
})

// [covers:frontend.ui-refresh-interval-localstorage]
describe('UI更新間隔設定のfrontend側保存(ui-refresh store)', () => {
  it('未保存時は既定10秒(10000ms)', () => {
    const fetchCalls = stubFetchCounter()
    const ui = useUiRefreshStore()
    ui.load()
    expect(ui.intervalMs).toBe(10000)
    expect(fetchCalls).toEqual([])
  })

  it('保存値を読み込みsetIntervalMsでlocalStorageへ保存する(server設定に依存しない)', () => {
    const fetchCalls = stubFetchCounter()
    storage.setItem(STORAGE_KEY, '5000')
    const ui = useUiRefreshStore()
    ui.load()
    expect(ui.intervalMs).toBe(5000)
    ui.setIntervalMs(30000)
    expect(ui.intervalMs).toBe(30000)
    expect(storage.getItem(STORAGE_KEY)).toBe('30000')
    expect(fetchCalls).toEqual([])
  })

  it('無効(null)も保存され別store実体のloadで復元される', () => {
    const ui = useUiRefreshStore()
    ui.setIntervalMs(null)
    expect(ui.intervalMs).toBeNull()
    expect(storage.getItem(STORAGE_KEY)).toBe('null')
    // 新規pinia上の別実体で保存値を読み直す(再読込相当)
    setActivePinia(createPinia())
    const reloaded = useUiRefreshStore()
    reloaded.load()
    expect(reloaded.intervalMs).toBeNull()
  })

  it('不正な保存値(非数値)は既定10000msへ戻す', () => {
    storage.setItem(STORAGE_KEY, 'abc')
    const ui = useUiRefreshStore()
    ui.load()
    expect(ui.intervalMs).toBe(10000)
  })
})

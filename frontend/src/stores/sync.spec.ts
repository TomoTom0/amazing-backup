import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createPinia, setActivePinia } from 'pinia'
import { useSyncStore } from './sync'

// sync storeのfetchHealth状態遷移(TASK-9設計7章・12章P2のliveness表示)

beforeEach(() => {
  setActivePinia(createPinia())
})

afterEach(() => {
  vi.unstubAllGlobals()
})

// [covers:frontend.health-state-store]
describe('sync storeのfetchHealth', () => {
  it('成功時はhealth=ok・message・lastError=nullへ遷移する', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(
        new Response(JSON.stringify({ status: 'ok' }), {
          status: 200,
          headers: { 'Content-Type': 'application/json' },
        }),
      ),
    )
    const sync = useSyncStore()
    await sync.fetchHealth()
    expect(sync.health).toBe('ok')
    expect(sync.message).toBe('ok')
    expect(sync.lastError).toBeNull()
  })

  it('失敗時はhealth=error・lastError(ApiError)を保持する', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockRejectedValue(new TypeError('fetch failed')),
    )
    const sync = useSyncStore()
    await sync.fetchHealth()
    expect(sync.health).toBe('error')
    expect(sync.message).toBe('')
    expect(sync.lastError).not.toBeNull()
    if (sync.lastError !== null) {
      expect(sync.lastError.code).toBe('network')
    }
  })
})

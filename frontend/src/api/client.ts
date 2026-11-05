// API fetch client(TASK-9設計6章のerror body形式 {"error":{"code","message"}} 対応)。
// dev時はVite dev server(40512)のproxy、本番(embedded配信)は同一originの/api/*。

/** API error body(設計6章指摘#14: 一般化messageのみ。内部errorの無加工文字列は不正) */
export type ApiError = { code: string; message: string }

/** unknown -> Record<string, unknown>の最小型ガード */
function isRecord(v: unknown): v is Record<string, unknown> {
  return typeof v === 'object' && v !== null
}

/** API error body形式の判定 */
export function isApiErrorResponse(v: unknown): v is { error: ApiError } {
  if (!isRecord(v)) return false
  const error = v['error']
  if (!isRecord(error)) return false
  return typeof error['code'] === 'string' && typeof error['message'] === 'string'
}

/** GET /api/health の応答形式 */
export type HealthResponse = { status: string }

export function isHealthResponse(v: unknown): v is HealthResponse {
  return isRecord(v) && typeof v['status'] === 'string'
}

/** GET /api/sync の応答形式(TASK-9設計6章: sync状態。未sync時はstatus="unknown"・
 *  各時刻fieldは空文字列。型ガードで検証し不一致はinternal扱いにする) */
export type SyncStateResponse = {
  scope: string
  status: string
  index_etag: string
  index_version_id: string
  fetched_at: string
  last_write_at: string
  last_verified_at: string
  entry_count: number
}

export function isSyncStateResponse(v: unknown): v is SyncStateResponse {
  if (!isRecord(v)) return false
  const fields = [
    'scope',
    'status',
    'index_etag',
    'index_version_id',
    'fetched_at',
    'last_write_at',
    'last_verified_at',
  ]
  return (
    fields.every((f) => typeof v[f] === 'string') &&
    typeof v['entry_count'] === 'number'
  )
}

/** sync状態の取得(保護API。P2〜P3はfail-closed 503のためerror扱いになる) */
export function fetchSyncState(): Promise<ApiResult<SyncStateResponse>> {
  return apiGet('/api/sync', isSyncStateResponse)
}

/** API呼び出し結果: 成功(型ガード通過data)または失敗(ApiError+HTTP status) */
export type ApiResult<T> =
  | { ok: true; data: T }
  | { ok: false; error: ApiError; httpStatus: number }

/** GET共通。応答bodyは型ガードで検証し不一致はinternal扱いにする */
export async function apiGet<T>(
  path: string,
  guard: (v: unknown) => v is T,
): Promise<ApiResult<T>> {
  let resp: Response
  try {
    resp = await fetch(path, { headers: { Accept: 'application/json' } })
  } catch {
    return {
      ok: false,
      error: { code: 'network', message: 'serverに接続できません' },
      httpStatus: 0,
    }
  }
  const body: unknown = await resp
    .json()
    .catch(() => null)
  if (resp.ok) {
    if (guard(body)) {
      return { ok: true, data: body }
    }
    return {
      ok: false,
      error: { code: 'internal', message: '応答の形式が不正です' },
      httpStatus: resp.status,
    }
  }
  if (isApiErrorResponse(body)) {
    return { ok: false, error: body.error, httpStatus: resp.status }
  }
  return {
    ok: false,
    error: { code: 'unexpected_status', message: `HTTP ${resp.status}` },
    httpStatus: resp.status,
  }
}

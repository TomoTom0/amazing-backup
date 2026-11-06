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

/** 手動sync refresh(POST /api/sync・request body無し。応答はGET /api/syncと同一の
 * sync状態形式=server.sync-refresh-endpointの解釈に整合) */
export async function postSyncRefresh(): Promise<ApiResult<SyncStateResponse>> {
  let resp: Response
  try {
    resp = await fetch('/api/sync', {
      method: 'POST',
      headers: { Accept: 'application/json' },
    })
  } catch {
    return {
      ok: false,
      error: { code: 'network', message: 'serverに接続できません' },
      httpStatus: 0,
    }
  }
  const body: unknown = await resp.json().catch(() => null)
  if (resp.ok) {
    if (isSyncStateResponse(body)) {
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

/** GET /api/auth/status の応答形式(TASK-9設計6章: 認証状態と登録credential数。
 *  api_keys(登録api-key数・TASK-13追加)は応答に含まれない場合はundefined) */
export type AuthStatusResponse = {
  authenticated: boolean
  credentials: number
  api_keys?: number
}

export function isAuthStatusResponse(v: unknown): v is AuthStatusResponse {
  if (!isRecord(v)) return false
  if (typeof v['authenticated'] !== 'boolean') return false
  if (typeof v['credentials'] !== 'number') return false
  const apiKeys = v['api_keys']
  return apiKeys === undefined || typeof apiKeys === 'number'
}

/** 認証状態の取得(認証不要endpoint。LoginViewのUI誘導基盤) */
export function fetchAuthStatus(): Promise<ApiResult<AuthStatusResponse>> {
  return apiGet('/api/auth/status', isAuthStatusResponse)
}

// --- pending catalog(TASK-9設計6章・7章・12章P5。PendingViewから使用) ---

/** GET /api/pending のitem schema(source_root/path/size/mtime/status/last_scan_at) */
export type PendingItem = {
  source_root: string
  path: string
  size: number
  mtime: string
  status: string
  last_scan_at: string
}

/** GET /api/pending の応答形式({"pending":[...]}・statusは4状態) */
export type PendingResponse = { pending: PendingItem[] }

function isPendingItem(v: unknown): v is PendingItem {
  if (!isRecord(v)) return false
  const fields = ['source_root', 'path', 'mtime', 'status', 'last_scan_at']
  return fields.every((f) => typeof v[f] === 'string') && typeof v['size'] === 'number'
}

/** GET /api/pending応答の型ガード(pending配列+item schema一致のみtrue) */
export function isPendingResponse(v: unknown): v is PendingResponse {
  if (!isRecord(v) || !Array.isArray(v['pending'])) return false
  return v['pending'].every((item) => isPendingItem(item))
}

/** アップロード前カタログ一覧の取得(保護API。失敗時はerror扱い) */
export function fetchPending(): Promise<ApiResult<PendingResponse>> {
  return apiGet('/api/pending', isPendingResponse)
}

/** scan実行(POST /api/pending/scan・request body無し。応答はscan実行後の最新一覧=
 *  GET /api/pendingと同一形式) */
export async function postPendingScan(): Promise<ApiResult<PendingResponse>> {
  let resp: Response
  try {
    resp = await fetch('/api/pending/scan', {
      method: 'POST',
      headers: { Accept: 'application/json' },
    })
  } catch {
    return {
      ok: false,
      error: { code: 'network', message: 'serverに接続できません' },
      httpStatus: 0,
    }
  }
  const body: unknown = await resp.json().catch(() => null)
  if (resp.ok) {
    if (isPendingResponse(body)) {
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

// --- diff(TASK-9設計6章(endpoint一覧: GET/POST /api/diff)・7章・12章P6。
//     DiffViewから使用。応答はcmd_diffのJSON語彙{performed_at,entries,files,
//     missing,unexpected}・server.diff-endpointsの解釈に整合) ---

/** GET /api/diff のfiles要素schema({uuid,title}=索引entityの要約) */
export type DiffFile = { uuid: string; title: string }

/** GET/POST /api/diff の応答形式(diff結果) */
export type DiffResponse = {
  performed_at: string
  entries: number
  files: DiffFile[]
  missing: string[]
  unexpected: string[]
}

/** GET /api/diff応答の型ガード(cmd_diffのJSON語彙との一致のみtrue) */
export function isDiffResponse(v: unknown): v is DiffResponse {
  if (!isRecord(v)) return false
  if (typeof v['performed_at'] !== 'string') return false
  if (typeof v['entries'] !== 'number') return false
  const files = v['files']
  if (!Array.isArray(files)) return false
  const isFile = (f: unknown): f is DiffFile =>
    isRecord(f) && typeof f['uuid'] === 'string' && typeof f['title'] === 'string'
  if (!files.every(isFile)) return false
  const missing = v['missing']
  if (!Array.isArray(missing) || !missing.every((k) => typeof k === 'string')) return false
  const unexpected = v['unexpected']
  if (!Array.isArray(unexpected) || !unexpected.every((k) => typeof k === 'string')) return false
  return true
}

/** 最終diff結果の取得(保護API。未実行diffは404 not_foundのerror扱い) */
export function fetchDiff(): Promise<ApiResult<DiffResponse>> {
  return apiGet('/api/diff', isDiffResponse)
}

/** diff実行(POST /api/diff・request body無し。応答はdiff結果そのもののため
 * 実行後の一覧再GETは行わない=POST応答をresultへ保持する) */
export async function postDiffRun(): Promise<ApiResult<DiffResponse>> {
  let resp: Response
  try {
    resp = await fetch('/api/diff', {
      method: 'POST',
      headers: { Accept: 'application/json' },
    })
  } catch {
    return {
      ok: false,
      error: { code: 'network', message: 'serverに接続できません' },
      httpStatus: 0,
    }
  }
  const body: unknown = await resp.json().catch(() => null)
  if (resp.ok) {
    if (isDiffResponse(body)) {
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

/** POST /api/auth/logout(session破棄。未認証401でもUI状態は未認証へ揃える) */
export async function postLogout(): Promise<ApiResult<Record<string, unknown>>> {
  let resp: Response
  try {
    resp = await fetch('/api/auth/logout', {
      method: 'POST',
      headers: { Accept: 'application/json' },
    })
  } catch {
    return {
      ok: false,
      error: { code: 'network', message: 'serverに接続できません' },
      httpStatus: 0,
    }
  }
  const body: unknown = await resp.json().catch(() => null)
  if (resp.ok) {
    if (isRecord(body)) {
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

// --- entities照会(TASK-9設計6章(GET /api/entities?query=&category=&label=&page=
//     &scope=・:uuid・:uuid/versions)・7章(EntitiesView・EntityDetailView)・
//     コードレビュー指摘高4。応答wrap・要素schemaはserver.entities-list-endpoint・
//     entity-detail-endpoint・entity-versions-endpointの解釈に整合) ---

/** entityのlocation 1件分(entry_summary_json互換の語彙) */
export type EntityLocation = {
  source_path: string
  category: string
  label: string
  identity_hash: string
  last_backup: string
}

/** entityのpin 1件分 */
export type EntityPin = { pin: string; date: string; source_size: number }

/** entity要約(GET /api/entitiesの要素=entry_summary語彙。versionsは版数) */
export type EntitySummary = {
  uuid: string
  title: string
  locations: EntityLocation[]
  versions: number
  latest_date: string
  latest_size: number
  pins: EntityPin[]
}

/** GET /api/entities の応答形式({"entities":[...],"page":n,"total_pages":n}) */
export type EntitiesResponse = {
  entities: EntitySummary[]
  page: number
  total_pages: number
}

function isEntityLocation(v: unknown): v is EntityLocation {
  if (!isRecord(v)) return false
  const fields = ['source_path', 'category', 'label', 'identity_hash', 'last_backup']
  return fields.every((f) => typeof v[f] === 'string')
}

function isEntityPin(v: unknown): v is EntityPin {
  return (
    isRecord(v) &&
    typeof v['pin'] === 'string' &&
    typeof v['date'] === 'string' &&
    typeof v['source_size'] === 'number'
  )
}

function isEntitySummary(v: unknown): v is EntitySummary {
  if (!isRecord(v)) return false
  if (typeof v['uuid'] !== 'string' || typeof v['title'] !== 'string') return false
  if (!Array.isArray(v['locations']) || !v['locations'].every(isEntityLocation)) {
    return false
  }
  if (typeof v['versions'] !== 'number') return false
  if (typeof v['latest_date'] !== 'string' || typeof v['latest_size'] !== 'number') {
    return false
  }
  return Array.isArray(v['pins']) && v['pins'].every(isEntityPin)
}

/** GET /api/entities応答の型ガード(wrap+要素schema一致のみtrue) */
export function isEntitiesResponse(v: unknown): v is EntitiesResponse {
  if (!isRecord(v)) return false
  if (!Array.isArray(v['entities']) || !v['entities'].every(isEntitySummary)) {
    return false
  }
  return typeof v['page'] === 'number' && typeof v['total_pages'] === 'number'
}

/** entity詳細のversion_list要素(GET /api/entities/:uuidの詳細用) */
export type EntityVersionEntry = {
  date: string
  version_ts: string
  version_id: string
  batch_id: string
  source_size: number
}

/** GET /api/entities/:uuid の応答(要約+version_list付き=entity-detail-endpoint) */
export type EntityDetail = EntitySummary & { version_list: EntityVersionEntry[] }

function isEntityVersionEntry(v: unknown): v is EntityVersionEntry {
  if (!isRecord(v)) return false
  const fields = ['date', 'version_ts', 'version_id', 'batch_id']
  return fields.every((f) => typeof v[f] === 'string') && typeof v['source_size'] === 'number'
}

/** GET /api/entities/:uuid応答の型ガード(要約+version_list配列の一致のみtrue) */
export function isEntityDetailResponse(v: unknown): v is EntityDetail {
  if (!isEntitySummary(v)) return false
  return hasEntityVersionList(v)
}

function hasEntityVersionList(v: unknown): boolean {
  if (!isRecord(v)) return false
  const list = v['version_list']
  return Array.isArray(list) && list.every(isEntityVersionEntry)
}

/** GET /api/entities/:uuid/versions のversions要素(state=現行/noncurrent付き) */
export type EntityVersionsItem = {
  date: string
  version_ts: string
  version_id: string
  source_size: number
  state: string
}

/** GET /api/entities/:uuid/versions の応答(list versions --format json相当) */
export type EntityVersionsResponse = {
  uuid: string
  title: string
  locations: EntityLocation[]
  versions: EntityVersionsItem[]
  pins: EntityPin[]
}

function isEntityVersionsItem(v: unknown): v is EntityVersionsItem {
  if (!isRecord(v)) return false
  const fields = ['date', 'version_ts', 'version_id', 'state']
  return fields.every((f) => typeof v[f] === 'string') && typeof v['source_size'] === 'number'
}

/** GET /api/entities/:uuid/versions応答の型ガード */
export function isEntityVersionsResponse(v: unknown): v is EntityVersionsResponse {
  if (!isRecord(v)) return false
  if (typeof v['uuid'] !== 'string' || typeof v['title'] !== 'string') return false
  if (!Array.isArray(v['locations']) || !v['locations'].every(isEntityLocation)) return false
  if (!Array.isArray(v['versions']) || !v['versions'].every(isEntityVersionsItem)) {
    return false
  }
  return Array.isArray(v['pins']) && v['pins'].every(isEntityPin)
}

/** GET /api/entitiesの絞り込みparam(値のあるparamのみURL queryへ直列化する。
 * scopeは省略=server側実効profile) */
export type EntitiesQueryParams = {
  query?: string
  category?: string
  label?: string
  page?: number
}

/** entity一覧の取得(保護API。query/category/label/pageは値のあるもののみ付与) */
export function fetchEntities(
  params: EntitiesQueryParams = {},
): Promise<ApiResult<EntitiesResponse>> {
  const search = new URLSearchParams()
  if (params.query) search.set('query', params.query)
  if (params.category) search.set('category', params.category)
  if (params.label) search.set('label', params.label)
  if (params.page !== undefined) search.set('page', String(params.page))
  const qs = search.toString()
  return apiGet(qs === '' ? '/api/entities' : `/api/entities?${qs}`, isEntitiesResponse)
}

/** entity詳細の取得(保護API。不在uuidの404 not_foundはerror扱い) */
export function fetchEntityDetail(uuid: string): Promise<ApiResult<EntityDetail>> {
  return apiGet(`/api/entities/${uuid}`, isEntityDetailResponse)
}

/** entityの版一覧取得(保護API。state=現行/noncurrent付き) */
export function fetchEntityVersions(uuid: string): Promise<ApiResult<EntityVersionsResponse>> {
  return apiGet(`/api/entities/${uuid}/versions`, isEntityVersionsResponse)
}

// --- passkey登録・login(TASK-9設計8章・12章P4。LoginViewから使用) ---
// server応答はbase64url文字列のWebAuthn options。ここでは文字列のまま受け取り
// LoginViewでBufferSourceへ変換する(ArrayBufferとbase64urlの往来はUI層の責務)。

/** POST /api/auth/register/start の応答(PublicKeyCredentialCreationOptions相当) */
export type RegisterOptionsResponse = {
  publicKey: {
    challenge: string
    user: { id: string; name: string; displayName: string }
    pubKeyCredParams: { type: 'public-key'; alg: number }[]
  }
}

export function isRegisterOptionsResponse(v: unknown): v is RegisterOptionsResponse {
  if (!isRecord(v)) return false
  const pk = v['publicKey']
  if (!isRecord(pk)) return false
  if (typeof pk['challenge'] !== 'string') return false
  const user = pk['user']
  if (
    !isRecord(user) ||
    typeof user['id'] !== 'string' ||
    typeof user['name'] !== 'string' ||
    typeof user['displayName'] !== 'string'
  ) {
    return false
  }
  const params = pk['pubKeyCredParams']
  if (!Array.isArray(params)) return false
  return params.every(
    (p) => isRecord(p) && p['type'] === 'public-key' && typeof p['alg'] === 'number',
  )
}

/** POST /api/auth/login/start の応答(PublicKeyCredentialRequestOptions相当) */
export type LoginOptionsResponse = {
  publicKey: {
    challenge: string
    allowCredentials?: { type: 'public-key'; id: string }[]
  }
}

export function isLoginOptionsResponse(v: unknown): v is LoginOptionsResponse {
  if (!isRecord(v)) return false
  const pk = v['publicKey']
  if (!isRecord(pk) || typeof pk['challenge'] !== 'string') return false
  const allow = pk['allowCredentials']
  if (allow !== undefined && !Array.isArray(allow)) return false
  if (Array.isArray(allow)) {
    return allow.every(
      (c) => isRecord(c) && c['type'] === 'public-key' && typeof c['id'] === 'string',
    )
  }
  return true
}

/** POST共通(JSON body。応答は型ガードで検証し不一致はinternal扱い) */
async function apiPost<T>(
  path: string,
  requestBody: unknown,
  guard: (v: unknown) => v is T,
): Promise<ApiResult<T>> {
  let resp: Response
  try {
    resp = await fetch(path, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json', Accept: 'application/json' },
      body: JSON.stringify(requestBody),
    })
  } catch {
    return {
      ok: false,
      error: { code: 'network', message: 'serverに接続できません' },
      httpStatus: 0,
    }
  }
  const body: unknown = await resp.json().catch(() => null)
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

/** 登録開始(bootstrap token提示。登録UIはtoken提示時のみ開く=設計8章) */
export function postRegisterStart(
  token: string,
): Promise<ApiResult<RegisterOptionsResponse>> {
  return apiPost('/api/auth/register/start', { token }, isRegisterOptionsResponse)
}

/** 登録完了(WebAuthn登録応答。base64url文字列で送る) */
export function postRegisterFinish(credential: {
  id: string
  rawId: string
  type: string
  response: { attestationObject: string; clientDataJSON: string; transports?: string[] }
}): Promise<ApiResult<Record<string, unknown>>> {
  return apiPost(
    '/api/auth/register/finish',
    credential,
    (v): v is Record<string, unknown> => isRecord(v),
  )
}

/** login開始(challenge発行) */
export function postLoginStart(): Promise<ApiResult<LoginOptionsResponse>> {
  return apiPost('/api/auth/login/start', {}, isLoginOptionsResponse)
}

/** login完了(WebAuthn認証応答。base64url文字列で送る) */
export function postLoginFinish(credential: {
  id: string
  rawId: string
  type: string
  response: {
    authenticatorData: string
    clientDataJSON: string
    signature: string
    userHandle: string | null
  }
}): Promise<ApiResult<Record<string, unknown>>> {
  return apiPost(
    '/api/auth/login/finish',
    credential,
    (v): v is Record<string, unknown> => isRecord(v),
  )
}

/** POST /api/auth/api-key の応答(login finishと同一の{"status":"ok"}形式) */
export type StatusOkResponse = { status: string }

function isStatusOkResponse(v: unknown): v is StatusOkResponse {
  return isRecord(v) && typeof v['status'] === 'string'
}

/** api-key認証(TASK-13設計D13・2.4: {"api_key"}bodyでPOSTし既存sessionへ合流。
 *  401 unauthenticated(key不一致)・403 forbidden_source(LAN外)はerror扱いで
 *  codeをそのまま専用失敗表示へ返す。fetchは同一origin cookie既定のため
 *  session cookieはpasskey loginと同一扱い) */
export function postApiKeyAuth(apiKey: string): Promise<ApiResult<StatusOkResponse>> {
  return apiPost('/api/auth/api-key', { api_key: apiKey }, isStatusOkResponse)
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

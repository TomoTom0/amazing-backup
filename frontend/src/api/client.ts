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

/** entity要約(GET /api/entitiesの要素=entry_summary語彙。versionsは版数。
 *  W2(TASK-17)でtags(行のtag列chip表示)を追加) */
export type EntitySummary = {
  uuid: string
  title: string
  locations: EntityLocation[]
  versions: number
  latest_date: string
  latest_size: number
  pins: EntityPin[]
  tags: string[]
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
  if (!Array.isArray(v['pins']) || !v['pins'].every(isEntityPin)) return false
  return isStringArray(v['tags'])
}

/** string[]形式の判定(tags・collections等の配列field共通) */
function isStringArray(v: unknown): v is string[] {
  return Array.isArray(v) && v.every((item) => typeof item === 'string')
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

/** GET /api/entities/:uuid の応答(要約+version_list付き=entity-detail-endpoint。
 *  W2(TASK-17)でcollections(所属collection名一覧・詳細の所属collection表示)を追加) */
export type EntityDetail = EntitySummary & {
  collections: string[]
  version_list: EntityVersionEntry[]
}

function isEntityVersionEntry(v: unknown): v is EntityVersionEntry {
  if (!isRecord(v)) return false
  const fields = ['date', 'version_ts', 'version_id', 'batch_id']
  return fields.every((f) => typeof v[f] === 'string') && typeof v['source_size'] === 'number'
}

/** GET /api/entities/:uuid応答の型ガード(要約+version_list+collections配列の一致のみtrue) */
export function isEntityDetailResponse(v: unknown): v is EntityDetail {
  if (!isEntitySummary(v)) return false
  return hasEntityCollections(v) && hasEntityVersionList(v)
}

/** collections配列のfield検証(引数をunknownで受けisRecord経由でaccessする=
 *  isEntitySummary通過後のnarrow型(EntitySummary)にkeyが無いindex accessに
 *  ならないようにする=hasEntityVersionListと同一pattern) */
function hasEntityCollections(v: unknown): boolean {
  if (!isRecord(v)) return false
  return isStringArray(v['collections'])
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
 * scopeは省略=server側実効profile。W2(TASK-17)でtag/tag名・collection=collection id
 * (文字列化)の絞り込みparamを追加。W2レビュー指摘対応(TASK-19)でuntagged(tag無し
 * 絞り込み=true時untagged=1)を追加しlabelはラベルなし絞り込みのため空文字も直列化
 * する(値のあるparamのみ規則のlabel限定例外)) */
export type EntitiesQueryParams = {
  query?: string
  category?: string
  label?: string
  tag?: string
  collection?: string
  untagged?: boolean
  page?: number
}

/** entity一覧の取得(保護API。query/category/tag/collection/pageは値のあるもののみ
 * 付与。labelは空文字(ラベルなし完全一致)も送る・untagged=true時はuntagged=1を付与
 * (tag=との同時指定はserver側400のため呼び出し側で排他する)。folder絞り込みは
 * label=で成立するため専用paramは設けない) */
export function fetchEntities(
  params: EntitiesQueryParams = {},
): Promise<ApiResult<EntitiesResponse>> {
  const search = new URLSearchParams()
  if (params.query) search.set('query', params.query)
  if (params.category) search.set('category', params.category)
  if (params.label !== undefined) search.set('label', params.label)
  if (params.tag) search.set('tag', params.tag)
  if (params.collection) search.set('collection', params.collection)
  if (params.untagged) search.set('untagged', '1')
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

// --- runs/jobs(TASK-17設計2.1(ホームbackup概况・実行中job区画)・3.2(GET /api/runs・
//     GET /api/jobs)・9章W1。応答schemaはserver.runs-list・server.jobs-listの解釈に整合) ---

/** GET /api/runs のrun schema(batch_id=run識別・date=runの日付・entity_count=対象実体数) */
export type RunItem = { batch_id: string; date: string; entity_count: number }

/** GET /api/runs の応答形式({"runs":[...]}・date降順=直近runが先頭) */
export type RunsResponse = { runs: RunItem[] }

/** GET /api/runs応答の型ガード(runs配列+各要素のbatch_id/date/entity_count一致のみtrue) */
export function isRunsResponse(v: unknown): v is RunsResponse {
  if (!isRecord(v) || !Array.isArray(v['runs'])) return false
  return v['runs'].every(
    (r) =>
      isRecord(r) &&
      typeof r['batch_id'] === 'string' &&
      typeof r['date'] === 'string' &&
      typeof r['entity_count'] === 'number',
  )
}

/** run一覧の取得(保護API。ホームの前回backup表示基盤。失敗時はerror扱い) */
export function fetchRuns(): Promise<ApiResult<RunsResponse>> {
  return apiGet('/api/runs', isRunsResponse)
}

/** GET /api/jobs のjob schema(W1の最小schema。params/progress/result/errorの詳細
 *  fieldはW4のjob実装時に拡張) */
export type JobItem = {
  id: number
  kind: string
  status: string
  created_at: string
  updated_at: string
}

/** GET /api/jobs の応答形式({"jobs":[...]}・W1のserver応答は常に空配列) */
export type JobsResponse = { jobs: JobItem[] }

/** GET /api/jobs応答の型ガード(jobs配列+各要素のid/kind/status/created_at/
 *  updated_at一致のみtrue) */
export function isJobsResponse(v: unknown): v is JobsResponse {
  if (!isRecord(v) || !Array.isArray(v['jobs'])) return false
  return v['jobs'].every(
    (j) =>
      isRecord(j) &&
      typeof j['id'] === 'number' &&
      typeof j['kind'] === 'string' &&
      typeof j['status'] === 'string' &&
      typeof j['created_at'] === 'string' &&
      typeof j['updated_at'] === 'string',
  )
}

/** job一覧の取得(保護API。W1のserver応答は常に空配列。失敗時はerror扱い) */
export function fetchJobs(): Promise<ApiResult<JobsResponse>> {
  return apiGet('/api/jobs', isJobsResponse)
}

// --- tags/folders/collections(TASK-17設計2.2(バックアップ一覧の3ビュー)・3.2
//     (GET /api/tags・folders・collections(+/:id)・PUT /api/entities/:uuid/tags)・
//     4.3(folder=location.labelの階層集約・tag/collectionはlocal語彙)・9章W2。
//     一覧要素のentity_count(2026-11-14のAPI契約統一裁定: collections一覧の
//     entity_count=構成entity数)を含む) ---

/** GET /api/tags のtag schema(tag名と当該tagを持つentity数) */
export type TagItem = { tag: string; entity_count: number }

/** GET /api/tags の応答形式({"tags":[...]}・tag名昇順) */
export type TagsResponse = { tags: TagItem[] }

/** GET /api/tags応答の型ガード(tags配列+各要素のtag/entity_count一致のみtrue) */
export function isTagsResponse(v: unknown): v is TagsResponse {
  if (!isRecord(v) || !Array.isArray(v['tags'])) return false
  return v['tags'].every(
    (t) => isRecord(t) && typeof t['tag'] === 'string' && typeof t['entity_count'] === 'number',
  )
}

/** tag一覧の取得(保護API。tagビューの選択肢表示基盤。失敗時はerror扱い) */
export function fetchTags(): Promise<ApiResult<TagsResponse>> {
  return apiGet('/api/tags', isTagsResponse)
}

/** GET /api/folders のfolder schema(label=location.label・path='/'区切りの
 *  階層path・entity_count=当該folder直下のentity数) */
export type FolderItem = { label: string; path: string; entity_count: number }

/** GET /api/folders の応答形式({"folders":[...]}・階層集約済み) */
export type FoldersResponse = { folders: FolderItem[] }

/** GET /api/folders応答の型ガード(folders配列+各要素のlabel/path/entity_count
 *  一致のみtrue) */
export function isFoldersResponse(v: unknown): v is FoldersResponse {
  if (!isRecord(v) || !Array.isArray(v['folders'])) return false
  return v['folders'].every(
    (f) =>
      isRecord(f) &&
      typeof f['label'] === 'string' &&
      typeof f['path'] === 'string' &&
      typeof f['entity_count'] === 'number',
  )
}

/** folder一覧の取得(保護API。folder構造ビューのtree表示基盤。失敗時はerror扱い) */
export function fetchFolders(): Promise<ApiResult<FoldersResponse>> {
  return apiGet('/api/folders', isFoldersResponse)
}

/** GET /api/collections のcollection schema(id/name+構成entity数) */
export type CollectionItem = { id: number; name: string; entity_count: number }

/** GET /api/collections の応答形式({"collections":[...]}・name昇順) */
export type CollectionsResponse = { collections: CollectionItem[] }

/** GET /api/collections応答の型ガード(collections配列+各要素のid/name/entity_count
 *  一致のみtrue) */
export function isCollectionsResponse(v: unknown): v is CollectionsResponse {
  if (!isRecord(v) || !Array.isArray(v['collections'])) return false
  return v['collections'].every(
    (c) =>
      isRecord(c) &&
      typeof c['id'] === 'number' &&
      typeof c['name'] === 'string' &&
      typeof c['entity_count'] === 'number',
  )
}

/** GET /api/collections/:id のcollection詳細schema(id/name+構成uuid一覧) */
export type CollectionDetail = { id: number; name: string; uuids: string[] }

/** GET /api/collections/:id の応答形式(詳細schema。POST/PUT collections・items追加の
 *  書込応答と同一形式=design doc 3.1「読取系と同一schema」原則) */
export type CollectionDetailResponse = { collection: CollectionDetail }

/** GET /api/collections/:id応答の型ガード(collection wrap+id/name/uuids配列一致
 *  のみtrue) */
export function isCollectionDetailResponse(v: unknown): v is CollectionDetailResponse {
  if (!isRecord(v)) return false
  const c = v['collection']
  if (!isRecord(c)) return false
  if (typeof c['id'] !== 'number' || typeof c['name'] !== 'string') return false
  return isStringArray(c['uuids'])
}

/** collection一覧の取得(保護API。collectionビューの選択肢表示基盤) */
export function fetchCollections(): Promise<ApiResult<CollectionsResponse>> {
  return apiGet('/api/collections', isCollectionsResponse)
}

/** collection詳細(構成uuid一覧)の取得(保護API。不在idの404 not_foundはerror扱い) */
export function fetchCollectionDetail(id: number): Promise<ApiResult<CollectionDetailResponse>> {
  return apiGet(`/api/collections/${id}`, isCollectionDetailResponse)
}

/** entityのtag一覧置換(PUT /api/entities/:uuid/tags。body {tags:[...]}で丸ごと置換=
 *  冪等。応答は操作後の対象状態(読取系と同一schema=design doc 3.1方針)のため
 *  操作後の再GETは不要。tag名規則違反は422・不在uuidは404) */
export function putEntityTags(uuid: string, tags: string[]): Promise<ApiResult<EntityDetail>> {
  return apiPut(`/api/entities/${uuid}/tags`, { tags }, isEntityDetailResponse)
}

/** W3(TASK-22)書込APIの応答: EntityDetail形式+title重複時のみwarning(CLI
 *  warn_if_title_duplicateと同一文言)。warningは省略可(重複無し時は不在) */
export type EntityMutationResponse = EntityDetail & { warning?: string }

/** 書込API応答の型ガード(EntityDetail形式+warningはstringまたは不在)。warning検査は
 * unknown経由でindex accessする(isEntityDetailResponse通過後のnarrow型にwarning keyが
 * 無いため=hasEntityCollectionsと同一pattern) */
export function isEntityMutationResponse(v: unknown): v is EntityMutationResponse {
  return isEntityDetailResponse(v) && hasOptionalWarning(v)
}

/** warning fieldの検査(不在またはstring) */
function hasOptionalWarning(v: unknown): boolean {
  if (!isRecord(v)) return false
  const warning: unknown = v['warning']
  return warning === undefined || typeof warning === 'string'
}

/** entityのtitle変更(POST /api/entities/:uuid/rename。body {title}。応答は操作後の
 *  対象状態+title重複時のみwarning=許容の警告。title規則違反は422・不在uuidは404) */
export function renameEntity(
  uuid: string,
  title: string,
): Promise<ApiResult<EntityMutationResponse>> {
  return apiPost(`/api/entities/${uuid}/rename`, { title }, isEntityMutationResponse)
}

/** entityの所在の分類変更(POST /api/entities/:uuid/location。body
 *  {source_path, category, label}。対象locationはsource_path完全一致。不在は404・
 *  category/label規則違反は422。応答は操作後の対象状態) */
export function updateEntityLocation(
  uuid: string,
  sourcePath: string,
  category: string,
  label: string,
): Promise<ApiResult<EntityDetail>> {
  return apiPost(
    `/api/entities/${uuid}/location`,
    { source_path: sourcePath, category, label },
    isEntityDetailResponse,
  )
}

// --- collection書込(W2レビュー指摘高3=TASK-19。API契約は2026-11-14確定分に固定:
//     POST /api/collections body{name}応答{collection:{id,name,uuids}}・
//     PUT /api/collections/:id body{name}・DELETE /api/collections/:id応答
//     {status:"ok"}・POST /api/collections/:id/items body{uuids}応答詳細schema・
//     DELETE /api/collections/:id/items/:uuid応答詳細schema) ---

/** collection作成(POST /api/collections。応答は詳細schema=作成後の対象状態) */
export function createCollection(name: string): Promise<ApiResult<CollectionDetailResponse>> {
  return apiPost('/api/collections', { name }, isCollectionDetailResponse)
}

/** collection改名(PUT /api/collections/:id。応答は詳細schema=操作後の対象状態) */
export function renameCollection(
  id: number,
  name: string,
): Promise<ApiResult<CollectionDetailResponse>> {
  return apiPut(`/api/collections/${id}`, { name }, isCollectionDetailResponse)
}

/** collection削除(DELETE /api/collections/:id。応答は{"status":"ok"}形式) */
export function deleteCollection(id: number): Promise<ApiResult<StatusOkResponse>> {
  return apiDelete(`/api/collections/${id}`, isStatusOkResponse)
}

/** collection構成へのentity追加(POST /api/collections/:id/items。body {uuids}。
 *  応答は詳細schema=操作後の構成uuid一覧) */
export function addCollectionItems(
  id: number,
  uuids: string[],
): Promise<ApiResult<CollectionDetailResponse>> {
  return apiPost(`/api/collections/${id}/items`, { uuids }, isCollectionDetailResponse)
}

/** collection構成からのentity除去(DELETE /api/collections/:id/items/:uuid。
 *  応答は詳細schema=操作後の構成uuid一覧) */
export function removeCollectionItem(
  id: number,
  uuid: string,
): Promise<ApiResult<CollectionDetailResponse>> {
  return apiDelete(`/api/collections/${id}/items/${uuid}`, isCollectionDetailResponse)
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

/** PUT共通(JSON body。応答は型ガードで検証し不一致はinternal扱い=apiPostと同型。
 *  W2(TASK-17)のtag一覧置換(PUT /api/entities/:uuid/tags)で使用) */
async function apiPut<T>(
  path: string,
  requestBody: unknown,
  guard: (v: unknown) => v is T,
): Promise<ApiResult<T>> {
  let resp: Response
  try {
    resp = await fetch(path, {
      method: 'PUT',
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

/** DELETE共通(request body無し。応答は型ガードで検証し不一致はinternal扱い=
 *  apiGetと同型。W2レビュー指摘対応(TASK-19)のcollection削除・構成除去で使用) */
async function apiDelete<T>(
  path: string,
  guard: (v: unknown) => v is T,
): Promise<ApiResult<T>> {
  let resp: Response
  try {
    resp = await fetch(path, {
      method: 'DELETE',
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

<script setup lang="ts">
import { onMounted, ref } from 'vue'
import { useRouter } from 'vue-router'
import { useAuthStore } from '../stores/auth'
import {
  postRegisterFinish,
  postRegisterStart,
  postLoginFinish,
  postLoginStart,
} from '../api/client'

// TASK-9設計7章(LoginView)・8章(bootstrap token入力・登録UIはtoken提示時のみ)・
// 12章P4。auth storeのauthUi getterがUI誘導状態(register/login/authenticated)を
// 持ち、この画面はその表示とWebAuthn API呼出を担う(実browser観測が主のため
// vitest対象はstore/client層=frontend.toml参照)。

const auth = useAuthStore()
const router = useRouter()
/** bootstrap token入力(serve --bootstrapのstdout値)
 * 入力値はAPI呼び出し時のみ使用しlog等へ出さない */
const token = ref('')
const busy = ref(false)
const message = ref('')

onMounted(() => {
  void auth.fetchAuthStatus()
})

/** base64url文字列 -> BufferSource(WebAuthn API入力用) */
function b64urlToBytes(s: string): Uint8Array<ArrayBuffer> {
  const pad = s.length % 4 === 0 ? '' : '='.repeat(4 - (s.length % 4))
  const b64 = s.replace(/-/g, '+').replace(/_/g, '/') + pad
  const bin = atob(b64)
  const buffer = new ArrayBuffer(bin.length)
  const bytes = new Uint8Array(buffer)
  for (let i = 0; i < bin.length; i += 1) {
    bytes[i] = bin.charCodeAt(i)
  }
  return bytes
}

/** ArrayBuffer -> base64url文字列(server応答用) */
function bufferToB64url(buf: ArrayBuffer): string {
  const bytes = new Uint8Array(buf)
  let bin = ''
  bytes.forEach((b) => {
    bin += String.fromCharCode(b)
  })
  return btoa(bin).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '')
}

/** WebAuthn API応答の型ガード(navigator.credentialsの戻り値絞り込み) */
function isPublicKeyCredential(v: Credential | null): v is PublicKeyCredential {
  return v !== null && 'rawId' in v && 'response' in v
}

function isAttestationResponse(v: AuthenticatorResponse): v is AuthenticatorAttestationResponse {
  return 'attestationObject' in v
}

function isAssertionResponse(v: AuthenticatorResponse): v is AuthenticatorAssertionResponse {
  return 'authenticatorData' in v
}

function fail(code: string, httpStatus: number): void {
  if (code === 'unauthenticated') {
    message.value =
      httpStatus === 401
        ? 'tokenが不正か期限切れです(serve --bootstrapで再発行してください)'
        : '認証に失敗しました'
    return
  }
  message.value = code === 'network' ? 'serverに接続できません' : `エラーが発生しました(${code})`
}

/** passkey登録(token提示時のみ=設計8章) */
async function register(): Promise<void> {
  if (busy.value || token.value === '') {
    if (token.value === '') message.value = 'bootstrap tokenを入力してください'
    return
  }
  busy.value = true
  message.value = ''
  const started = await postRegisterStart(token.value)
  if (!started.ok) {
    busy.value = false
    fail(started.error.code, started.httpStatus)
    return
  }
  const pk = started.data.publicKey
  const credential = await navigator.credentials.create({
    publicKey: {
      challenge: b64urlToBytes(pk.challenge),
      rp: { name: 'amazing-backup' },
      user: {
        id: b64urlToBytes(pk.user.id),
        name: pk.user.name,
        displayName: pk.user.displayName,
      },
      pubKeyCredParams: pk.pubKeyCredParams.map((p) => ({ type: p.type, alg: p.alg })),
    },
  })
  if (
    !isPublicKeyCredential(credential) ||
    !isAttestationResponse(credential.response)
  ) {
    busy.value = false
    message.value = 'passkey登録が中断されました'
    return
  }
  const finished = await postRegisterFinish({
    id: credential.id,
    rawId: bufferToB64url(credential.rawId),
    type: credential.type,
    response: {
      attestationObject: bufferToB64url(credential.response.attestationObject),
      clientDataJSON: bufferToB64url(credential.response.clientDataJSON),
    },
  })
  busy.value = false
  if (!finished.ok) {
    fail(finished.error.code, finished.httpStatus)
    return
  }
  message.value = 'passkeyを登録しました'
  await auth.fetchAuthStatus()
}

/** passkey login */
async function login(): Promise<void> {
  if (busy.value) return
  busy.value = true
  message.value = ''
  const started = await postLoginStart()
  if (!started.ok) {
    busy.value = false
    fail(started.error.code, started.httpStatus)
    return
  }
  const allow =
    started.data.publicKey.allowCredentials === undefined
      ? []
      : started.data.publicKey.allowCredentials.map((c) => ({
          type: c.type,
          id: b64urlToBytes(c.id),
        }))
  const credential = await navigator.credentials.get({
    publicKey: {
      challenge: b64urlToBytes(started.data.publicKey.challenge),
      allowCredentials: allow,
    },
  })
  if (!isPublicKeyCredential(credential) || !isAssertionResponse(credential.response)) {
    busy.value = false
    message.value = 'loginが中断されました'
    return
  }
  const finished = await postLoginFinish({
    id: credential.id,
    rawId: bufferToB64url(credential.rawId),
    type: credential.type,
    response: {
      authenticatorData: bufferToB64url(credential.response.authenticatorData),
      clientDataJSON: bufferToB64url(credential.response.clientDataJSON),
      signature: bufferToB64url(credential.response.signature),
      userHandle:
        credential.response.userHandle === null
          ? null
          : bufferToB64url(credential.response.userHandle),
    },
  })
  busy.value = false
  if (!finished.ok) {
    fail(finished.error.code, finished.httpStatus)
    return
  }
  await auth.fetchAuthStatus()
  if (auth.authenticated) {
    await router.push('/')
  }
}

/** logout(session破棄) */
async function logout(): Promise<void> {
  if (busy.value) return
  busy.value = true
  await auth.logout()
  busy.value = false
  await auth.fetchAuthStatus()
}
</script>

<template>
  <section class="card">
    <h2>passkey認証</h2>

    <p v-if="auth.authUi === 'unknown'" class="muted">認証状態を確認しています...</p>
    <p v-else-if="auth.authUi === 'authenticated'" class="muted">
      認証済みです(登録passkey数: {{ auth.credentialCount }})
    </p>
    <p v-else-if="auth.authUi === 'register'" class="muted">
      最初のpasskeyが未登録です。amazing-backup serve --bootstrap が表示する
      one-time tokenを入力して登録してください(登録はtoken提示時のみ開きます)。
    </p>
    <p v-else class="muted">登録済みのpasskeyでloginします。</p>

    <template v-if="auth.authUi === 'register'">
      <label>
        bootstrap token
        <input v-model="token" type="password" autocomplete="off" placeholder="serve --bootstrap の表示値" />
      </label>
      <button type="button" :disabled="busy" @click="register()">passkeyを登録</button>
    </template>

    <button v-else-if="auth.authUi === 'login'" type="button" :disabled="busy" @click="login()">
      login
    </button>

    <button
      v-if="auth.authUi === 'authenticated'"
      type="button"
      :disabled="busy"
      @click="logout()"
    >
      logout
    </button>

    <p v-if="message" class="message">{{ message }}</p>
    <p v-if="auth.lastError" class="muted">最後のエラー: {{ auth.lastError.code }}</p>
  </section>
</template>

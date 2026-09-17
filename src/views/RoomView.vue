<script setup lang="ts">
import { onMounted, ref } from 'vue'

import StatusDashboard from '../components/StatusDashboard.vue'
import { refreshLogin, startLogin, useLogin } from '../composables/useLogin'
import { useRoomConnection } from '../composables/useRoomConnection'

onMounted(refreshLogin)

const { loggedIn, logout } = useLogin()
const { roomId, busy, status, errorMsg, recentRooms, locked, statusText, connect, disconnect } = useRoomConnection()

const loggingIn = ref(false)
const loginError = ref('')

// 退出登录：先断开直播连接，再清登录态（清不清登录态都不影响正在跑的会话，主动断开更干净）
async function handleLogout() {
  if (status.value.state === 'connected' || status.value.state === 'connecting') {
    await disconnect()
  }
  await logout()
}

/** 扫码登录：Rust 开一个窗口加载抖音登录页，登录成功后窗口自动关 */
async function handleLogin() {
  loginError.value = ''
  loggingIn.value = true
  try {
    await startLogin()
  } catch (e) {
    loginError.value = String(e)
  } finally {
    loggingIn.value = false
  }
}

/** 输入框只留数字：抖音直播间号是纯数字长串，从地址栏粘贴时常带空格或整个链接 */
function onRoomInput(e: Event) {
  roomId.value = (e.target as HTMLInputElement).value.replace(/\D/g, '')
}
</script>

<template>
  <div class="room-page">
    <div class="login-bar" :class="{ logged: loggedIn }">
      <template v-if="loggedIn">
        <span class="ok">✓ 已登录抖音（礼物消息可收）</span>
        <button class="link-btn" @click="handleLogout">退出登录</button>
      </template>
      <template v-else>
        <span class="warn">⚠ 未登录：弹幕 / 进场 / 关注 / 点赞照常接收，只有礼物消息需要登录</span>
        <button class="link-btn" :disabled="loggingIn" @click="handleLogin">
          {{ loggingIn ? '等待扫码…' : '扫码登录' }}
        </button>
      </template>
    </div>

    <p v-if="loginError" class="error">{{ loginError }}</p>

    <h2>直播间</h2>

    <div class="row">
      <input
        :value="roomId"
        class="room-input"
        type="text"
        inputmode="numeric"
        placeholder="输入直播间号，如 765790908625"
        :disabled="locked"
        @input="onRoomInput" />
      <button v-if="status.state === 'connected'" class="btn danger" :disabled="busy" @click="disconnect">断开</button>
      <button v-else class="btn primary" :disabled="busy || status.state === 'connecting'" @click="connect()">
        连接
      </button>
    </div>

    <div v-if="recentRooms.length" class="recent-bar">
      <span class="recent-label">最近</span>
      <button
        v-for="r in recentRooms"
        :key="r.room_id"
        class="crumb"
        :title="`房间 ${r.room_id}`"
        :disabled="busy || locked"
        @click="connect(r.room_id)">
        {{ r.uname || r.room_id }}
      </button>
    </div>

    <p v-if="errorMsg" class="error">{{ errorMsg }}</p>

    <div class="status-box">
      <span
        class="dot"
        :class="{
          green: status.state === 'connected',
          yellow: status.state === 'connecting',
          red: status.state === 'error',
          gray: status.state === 'disconnected',
        }"></span>
      <span>{{ statusText }}</span>
      <span v-if="status.state === 'connected'" class="room-tag">房间 {{ status.webRid }}（10s 心跳保活中）</span>
      <span v-if="status.state === 'error' && status.message" class="room-tag">
        {{ status.message }}
      </span>
    </div>

    <StatusDashboard />
  </div>
</template>

<style scoped>
.room-page {
  display: flex;
  flex-direction: column;
}

.login-bar {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
  background: var(--bg-side);
  border: 1px solid var(--border);
  border-radius: 6px;
  padding: 6px 10px;
  margin-bottom: 16px;
  font-size: 15px;
}

.login-bar .ok {
  color: var(--green);
}

.login-bar .warn {
  color: var(--yellow);
}

.link-btn {
  background: none;
  border: none;
  color: var(--accent);
  font-size: 15px;
  cursor: pointer;
  padding: 2px 6px;
  border-radius: 4px;
}

.link-btn:hover {
  background: var(--hover);
}

.link-btn:disabled {
  color: var(--text-faint);
  cursor: not-allowed;
}

.link-btn:disabled:hover {
  background: none;
}

h2 {
  font-size: 18px;
  margin-bottom: 14px;
}

.row {
  display: flex;
  gap: 8px;
}

.room-input {
  flex: 1;
  background: var(--bg-elev);
  border: 1px solid var(--border);
  border-radius: 6px;
  color: var(--text);
  padding: 8px 10px;
  font-size: 16px;
  outline: none;
}

.room-input:focus {
  border-color: var(--accent);
}

/* 锁定态与按钮禁用观感统一 */
.room-input:disabled {
  color: var(--text-faint);
  cursor: not-allowed;
  opacity: 0.7;
}

.btn {
  border: none;
  border-radius: 6px;
  padding: 8px 18px;
  font-size: 16px;
  color: #fff;
}

.btn.primary {
  background: var(--accent);
}

.btn.primary:hover {
  background: var(--accent-hover);
}

.btn.danger {
  background: var(--danger);
}

.btn.danger:hover {
  background: var(--danger-hover);
}

.btn:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}

.error {
  color: var(--red);
  font-size: 15px;
  margin-top: 8px;
}

/* 最近房间面包屑：点击直连；连接中/已连接时禁用（需先断开） */
.recent-bar {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 2px 8px;
  margin-top: 10px;
}

.recent-label {
  color: var(--text-faint);
  font-size: 14px;
}

.crumb {
  background: none;
  border: none;
  padding: 2px 0;
  color: var(--accent);
  font-size: 14px;
  cursor: pointer;
  max-width: 12em;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.crumb + .crumb::before {
  content: '›';
  color: var(--text-faint);
  margin-right: 8px;
}

.crumb:hover:not(:disabled) {
  text-decoration: underline;
}

.crumb:disabled {
  color: var(--text-faint);
  cursor: not-allowed;
}

.status-box {
  margin-top: 18px;
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 16px;
}

.dot {
  width: 9px;
  height: 9px;
  border-radius: 50%;
}

.dot.green {
  background: var(--green);
}
.dot.yellow {
  background: var(--yellow);
}
.dot.red {
  background: var(--red);
}
.dot.gray {
  background: var(--gray);
}

.room-tag {
  color: var(--text-faint);
  font-size: 14px;
}
</style>

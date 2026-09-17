// 隐藏签名页入口：把 webmssdk 的 getSign 包成「一次请求 → 一次回调」。
//
// Rust 侧用 `eval` 触发（见 src-tauri/src/douyin/signer.rs），但 `eval` 拿不到 JS 的返回值，
// 所以结果走命令 `douyin_sign_reply` 回传：{ id, bogus?, error? }。
// 页面还没加载完（getSign 未定义）时回 error='NOT_READY'，Rust 会短暂重试而不是直接报错。
import { invoke } from '@tauri-apps/api/core'

declare global {
  interface Window {
    getSign?: (param: { 'X-MS-STUB': string }) => { 'X-Bogus'?: string } | undefined
    __douyinSign?: (id: number, stub: string) => void
  }
}

window.__douyinSign = (id, stub) => {
  const reply = (payload: { bogus?: string | null; error?: string | null }) => {
    void invoke('douyin_sign_reply', { id, ...payload })
  }
  if (typeof window.getSign !== 'function') {
    reply({ error: 'NOT_READY' })
    return
  }
  try {
    const bogus = window.getSign({ 'X-MS-STUB': stub })?.['X-Bogus']
    if (bogus) reply({ bogus })
    else reply({ error: 'getSign 没返回 X-Bogus' })
  } catch (e) {
    reply({ error: String(e) })
  }
}

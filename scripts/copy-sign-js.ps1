# 把探针验证过的 webmssdk（probe/browser/sign_browser.js）拷成前端静态资源。
#
# 为什么不直接进仓库：它是 DanmuFree 逆向 webmssdk 的产物（第三方代码，473 KB），
# 见 Task/task_plan.md「关键问题 2」。故 public/douyin-sign.js 被 .gitignore 排除，
# 新克隆的仓库要跑一次本脚本才能启动（否则签名页报「douyin-sign.js 缺失」）。
#
# 含中文，请用 pwsh 跑：pwsh -File scripts/copy-sign-js.ps1
$ErrorActionPreference = 'Stop'

$src = Join-Path $PSScriptRoot '..\probe\browser\sign_browser.js'
$dst = Join-Path $PSScriptRoot '..\public\douyin-sign.js'

if (-not (Test-Path $src)) {
    Write-Error "找不到 $src —— 探针产物没落盘？见 probe/README.md 的生成方式"
    exit 1
}
New-Item -ItemType Directory -Force -Path (Split-Path $dst) | Out-Null
Copy-Item $src $dst -Force
"已复制 → $dst（$((Get-Item $dst).Length) 字节）"

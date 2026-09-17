# 生成应用图标（透明背景 + 圆角深底 + 青→粉渐变的「弹」字）
#
# 配色取自抖音的品牌色（青 #25F4EE / 粉 #FE2C55）：深底保证任务栏那种 32px 小尺寸下
# 字还认得出来，青粉渐变一眼有抖音味。字形用「弹」（弹幕），不复制任何平台 logo。
# 生成后交给 Tauri 出全套尺寸：pnpm tauri icon imgs/app-icon.png
param([string]$Out = "imgs/app-icon.png")

Add-Type -AssemblyName System.Drawing

$size = 1024
$bmp = New-Object System.Drawing.Bitmap($size, $size, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
$g.TextRenderingHint = [System.Drawing.Text.TextRenderingHint]::AntiAliasGridFit
$g.Clear([System.Drawing.Color]::Transparent)

# 圆角矩形路径（大圆角）
$radius = 200
$path = New-Object System.Drawing.Drawing2D.GraphicsPath
$d = $radius * 2
$path.AddArc(0, 0, $d, $d, 180, 90)
$path.AddArc($size - $d, 0, $d, $d, 270, 90)
$path.AddArc($size - $d, $size - $d, $d, $d, 0, 90)
$path.AddArc(0, $size - $d, $d, $d, 90, 90)
$path.CloseFigure()

# 近黑底（很轻的斜向渐变，避免大尺寸下像一块死平的色块）
$bg = New-Object System.Drawing.Drawing2D.LinearGradientBrush(
    (New-Object System.Drawing.Point(0, 0)),
    (New-Object System.Drawing.Point($size, $size)),
    [System.Drawing.Color]::FromArgb(255, 27, 29, 34),
    [System.Drawing.Color]::FromArgb(255, 14, 15, 18))
$g.FillPath($bg, $path)

# 「弹」字：青→粉渐变填充
$fg = New-Object System.Drawing.Drawing2D.LinearGradientBrush(
    (New-Object System.Drawing.Point([int]($size * 0.12), [int]($size * 0.12))),
    (New-Object System.Drawing.Point([int]($size * 0.88), [int]($size * 0.92))),
    [System.Drawing.Color]::FromArgb(255, 37, 244, 238),
    [System.Drawing.Color]::FromArgb(255, 254, 44, 85))
$font = New-Object System.Drawing.Font('Microsoft YaHei', 430, [System.Drawing.FontStyle]::Bold, [System.Drawing.GraphicsUnit]::Pixel)
$sf = New-Object System.Drawing.StringFormat
$sf.Alignment = 'Center'
$sf.LineAlignment = 'Center'
$rect = New-Object System.Drawing.RectangleF(0, -20, $size, $size)
$g.DrawString('弹', $font, $fg, $rect, $sf)

$dir = Split-Path $Out -Parent
if ($dir -and -not (Test-Path $dir)) { New-Item -ItemType Directory -Force -Path $dir | Out-Null }
$bmp.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png)
$g.Dispose(); $bmp.Dispose()
Write-Output "saved $Out (transparent bg, 抖音青粉配色)"

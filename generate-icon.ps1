#Requires -Version 5.1
<#
.SYNOPSIS
生成 DeepSeek Harness 桌面端应用图标源图（app-icon.png，1024x1024）。

.DESCRIPTION
解析 assets/whale.svg 中 WebUI 的鲸鱼路径（仅 M/C/Z 命令），绘制在深蓝渐变
圆角方块上，输出 Tauri 图标管线输入。产物交给
`pnpm dlx @tauri-apps/cli icon app-icon.png`（在仓库根目录运行）生成全套尺寸。
依赖 System.Drawing（仅 Windows）；图标已生成时无需重复运行。
#>
Add-Type -AssemblyName System.Drawing

$size = 1024
$output = Join-Path $PSScriptRoot 'app-icon.png'

$svg = Get-Content (Join-Path $PSScriptRoot 'assets\whale.svg') -Raw
$d = [regex]::Match($svg, '<path[^>]*\bd="([^"]+)"').Groups[1].Value
if (-not $d) { throw 'assets/whale.svg 中未找到鲸鱼路径' }

# --- tokenize the path data ---
$tokens = [System.Collections.Generic.List[object]]::new()
$tokenPattern = '[a-zA-Z]|-?\d+(?:\.\d+)?(?:[eE][+-]?\d+)?'
foreach ($m in [regex]::Matches($d, $tokenPattern)) {
    if ($m.Value -match '^[a-zA-Z]$') {
        $tokens.Add(@{ type = 'cmd'; value = $m.Value })
    } else {
        $tokens.Add(@{ type = 'num'; value = [double]$m.Value })
    }
}

# --- build the GraphicsPath (viewBox 50x50 coordinates) ---
$whalePath = [System.Drawing.Drawing2D.GraphicsPath]::new()
$i = 0
$cmd = ''
$current = @(0.0, 0.0)
while ($i -lt $tokens.Count) {
    if ($tokens[$i].type -eq 'cmd') {
        $cmd = $tokens[$i].value
        $i++
    }
    switch ($cmd) {
        'M' {
            $x = [double]$tokens[$i].value
            $y = [double]$tokens[$i + 1].value
            $whalePath.StartFigure()
            $current = @($x, $y)
            $i += 2
            $cmd = 'L'   # implicit lineto after M
        }
        'L' {
            $x = [double]$tokens[$i].value
            $y = [double]$tokens[$i + 1].value
            $whalePath.AddLine([float]$current[0], [float]$current[1], [float]$x, [float]$y)
            $current = @($x, $y)
            $i += 2
        }
        'C' {
            $x1 = [double]$tokens[$i].value
            $y1 = [double]$tokens[$i + 1].value
            $x2 = [double]$tokens[$i + 2].value
            $y2 = [double]$tokens[$i + 3].value
            $x = [double]$tokens[$i + 4].value
            $y = [double]$tokens[$i + 5].value
            $whalePath.AddBezier([float]$current[0], [float]$current[1], [float]$x1, [float]$y1, [float]$x2, [float]$y2, [float]$x, [float]$y)
            $current = @($x, $y)
            $i += 6
        }
        'Z' {
            $whalePath.CloseFigure()
            $cmd = ''
        }
        default { throw "不支持的路径命令: $cmd" }
    }
}

# --- draw ---
$bitmap = New-Object System.Drawing.Bitmap -ArgumentList $size, $size, ([System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
$graphics = [System.Drawing.Graphics]::FromImage($bitmap)
$graphics.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
$graphics.Clear([System.Drawing.Color]::Transparent)

$radius = [int]($size * 0.22)
$rect = New-Object System.Drawing.Rectangle(0, 0, $size, $size)
$diameter = $radius * 2
$roundRect = [System.Drawing.Drawing2D.GraphicsPath]::new()
$roundRect.AddArc($rect.X, $rect.Y, $diameter, $diameter, 180, 90)
$roundRect.AddArc($rect.Right - $diameter, $rect.Y, $diameter, $diameter, 270, 90)
$roundRect.AddArc($rect.Right - $diameter, $rect.Bottom - $diameter, $diameter, $diameter, 0, 90)
$roundRect.AddArc($rect.X, $rect.Bottom - $diameter, $diameter, $diameter, 90, 90)
$roundRect.CloseFigure()

$top = [System.Drawing.Color]::FromArgb(255, 255, 255, 255)
$bottom = [System.Drawing.Color]::FromArgb(255, 255, 255, 255)
$bgBrush = [System.Drawing.Drawing2D.LinearGradientBrush]::new($rect, $top, $bottom, [float]90)
$graphics.FillPath($bgBrush, $roundRect)

# whale centered, covering ~74% of the tile (the favicon itself has no padding)
$cover = 0.74
$scale = ($size * $cover) / 50.0
$graphics.TranslateTransform($size / 2, $size / 2)
$graphics.ScaleTransform($scale, $scale)
$graphics.TranslateTransform(-25, -25)
$graphics.FillPath([System.Drawing.Brushes]::Black, $whalePath)

$bitmap.Save($output, [System.Drawing.Imaging.ImageFormat]::Png)

# sanity: the black whale must occupy a meaningful share of the canvas
$black = 0
$step = 4
for ($x = 0; $x -lt $size; $x += $step) {
    for ($y = 0; $y -lt $size; $y += $step) {
        $pixel = $bitmap.GetPixel($x, $y)
        if ($pixel.R -lt 60 -and $pixel.G -lt 60 -and $pixel.B -lt 60) { $black++ }
    }
}
$ratio = $black / (($size / $step) * ($size / $step))
if ($ratio -lt 0.02) { throw "鲸鱼渲染异常：黑色像素占比仅 $([math]::Round($ratio * 100, 1))%" }

$graphics.Dispose()
$bgBrush.Dispose()
$whalePath.Dispose()
$roundRect.Dispose()
$bitmap.Dispose()

Write-Host "已生成: $output (黑色鲸鱼占比 $([math]::Round($ratio * 100, 1))%)"

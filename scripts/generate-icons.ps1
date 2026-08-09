# Generates all Tauri bundle icons for TokenTray from scratch.
# Produces: app-icon.png (1024x1024 source), icons/32x32.png,
# icons/128x128.png, icons/128x128@2x.png (256x256), icons/icon.ico

param(
    [string]$OutDir = (Join-Path $PSScriptRoot "..\src-tauri\icons")
)

Add-Type -AssemblyName System.Drawing

$size = 1024
$outDir = [System.IO.Path]::GetFullPath($OutDir)
New-Item -ItemType Directory -Force -Path $outDir | Out-Null

$bmp = New-Object System.Drawing.Bitmap($size, $size)
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
$g.TextRenderingHint = [System.Drawing.Text.TextRenderingHint]::AntiAlias

# Rounded-square background, indigo -> fuchsia gradient
$radius = 190
$d = $radius * 2
$rect = New-Object System.Drawing.Rectangle(0, 0, $size, $size)
$path = New-Object System.Drawing.Drawing2D.GraphicsPath
$path.AddArc($rect.X, $rect.Y, $d, $d, 180, 90)
$path.AddArc($rect.X + $size - $d, $rect.Y, $d, $d, 270, 90)
$path.AddArc($rect.X + $size - $d, $rect.Y + $size - $d, $d, $d, 0, 90)
$path.AddArc($rect.X, $rect.Y + $size - $d, $d, $d, 90, 90)
$path.CloseFigure()

$bgBrush = New-Object System.Drawing.Drawing2D.LinearGradientBrush(
    $rect,
    [System.Drawing.Color]::FromArgb(255, 67, 56, 202),
    [System.Drawing.Color]::FromArgb(255, 168, 85, 247),
    135
)
$g.FillPath($bgBrush, $path)

# "T" glyph in white, slightly offset up for optical centering
$font = New-Object System.Drawing.Font("Segoe UI", 560, [System.Drawing.FontStyle]::Bold, [System.Drawing.GraphicsUnit]::Pixel)
$format = New-Object System.Drawing.StringFormat
$format.Alignment = [System.Drawing.StringAlignment]::Center
$format.LineAlignment = [System.Drawing.StringAlignment]::Center
$g.DrawString("T", $font, [System.Drawing.Brushes]::White,
    (New-Object System.Drawing.RectangleF(0, 40, $size, $size)), $format)

$g.Dispose()
$bmp.Save((Join-Path $outDir "app-icon.png"), [System.Drawing.Imaging.ImageFormat]::Png)

function Save-Scaled([System.Drawing.Bitmap]$src, [string]$name, [int]$w, [int]$h) {
    $scaled = New-Object System.Drawing.Bitmap($src, $w, $h)
    $scaled.Save($name, [System.Drawing.Imaging.ImageFormat]::Png)
    $scaled.Dispose()
}

Save-Scaled $bmp (Join-Path $outDir "32x32.png") 32 32
Save-Scaled $bmp (Join-Path $outDir "128x128.png") 128 128
Save-Scaled $bmp (Join-Path $outDir "128x128@2x.png") 256 256

$bmp256 = New-Object System.Drawing.Bitmap($bmp, 256, 256)
$icon = [System.Drawing.Icon]::FromHandle($bmp256.GetHicon())
$fs = [System.IO.File]::Create((Join-Path $outDir "icon.ico"))
$icon.Save($fs)
$fs.Close()
$icon.Dispose()
$bmp256.Dispose()
$bmp.Dispose()

Write-Host "Icons written to $outDir"

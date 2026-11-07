$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing

$iconDir = Join-Path $PSScriptRoot '..\src-tauri\icons'
New-Item -ItemType Directory -Force $iconDir | Out-Null
$size = 256

$bitmap = New-Object System.Drawing.Bitmap $size, $size, ([System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
$graphics = [System.Drawing.Graphics]::FromImage($bitmap)
$graphics.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
$graphics.Clear([System.Drawing.Color]::Transparent)

$background = New-Object System.Drawing.Drawing2D.LinearGradientBrush (New-Object System.Drawing.Point 0, 0), (New-Object System.Drawing.Point $size, $size), ([System.Drawing.Color]::FromArgb(255, 58, 123, 236)), ([System.Drawing.Color]::FromArgb(255, 31, 78, 168))
$path = New-Object System.Drawing.Drawing2D.GraphicsPath
$radius = 52
$path.AddArc(8, 8, $radius, $radius, 180, 90)
$path.AddArc($size - 8 - $radius, 8, $radius, $radius, 270, 90)
$path.AddArc($size - 8 - $radius, $size - 8 - $radius, $radius, $radius, 0, 90)
$path.AddArc(8, $size - 8 - $radius, $radius, $radius, 90, 90)
$path.CloseFigure()
$graphics.FillPath($background, $path)

$white = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(255, 255, 255, 255))
$shade = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(255, 205, 222, 252))
$left = 64; $width = 128; $ellipse = 40
foreach ($top in 168, 128, 88) {
    $graphics.FillRectangle($shade, $left, $top, $width, 28)
    $graphics.FillEllipse($shade, $left, $top + 28 - $ellipse / 2, $width, $ellipse)
    $graphics.FillEllipse($white, $left, $top - $ellipse / 2, $width, $ellipse)
}

$pngPath = Join-Path $iconDir 'icon.png'
$bitmap.Save($pngPath, [System.Drawing.Imaging.ImageFormat]::Png)
$graphics.Dispose(); $bitmap.Dispose()

$png = [System.IO.File]::ReadAllBytes($pngPath)
$stream = New-Object System.IO.MemoryStream
$writer = New-Object System.IO.BinaryWriter $stream
$writer.Write([UInt16]0); $writer.Write([UInt16]1); $writer.Write([UInt16]1)
$writer.Write([Byte]0); $writer.Write([Byte]0); $writer.Write([Byte]0); $writer.Write([Byte]0)
$writer.Write([UInt16]1); $writer.Write([UInt16]32)
$writer.Write([UInt32]$png.Length); $writer.Write([UInt32]22)
$writer.Write($png)
[System.IO.File]::WriteAllBytes((Join-Path $iconDir 'icon.ico'), $stream.ToArray())
Write-Output "Icons written to $iconDir"

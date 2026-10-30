Add-Type -AssemblyName System.Drawing

$size = 1024
$bmp = New-Object System.Drawing.Bitmap($size, $size, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
$g.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
$g.Clear([System.Drawing.Color]::Transparent)

function New-RoundedPath([float]$x, [float]$y, [float]$w, [float]$h, [float]$r) {
    $p = New-Object System.Drawing.Drawing2D.GraphicsPath
    $d = $r * 2
    $p.AddArc($x, $y, $d, $d, 180, 90)
    $p.AddArc($x + $w - $d, $y, $d, $d, 270, 90)
    $p.AddArc($x + $w - $d, $y + $h - $d, $d, $d, 0, 90)
    $p.AddArc($x, $y + $h - $d, $d, $d, 90, 90)
    $p.CloseFigure()
    return $p
}

# Тень
$shadow = New-RoundedPath 58 66 908 908 210
$shadowBrush = New-Object System.Drawing.Drawing2D.PathGradientBrush($shadow)
$shadowBrush.CenterColor = [System.Drawing.Color]::FromArgb(90, 10, 20, 45)
$shadowBrush.SurroundColors = @([System.Drawing.Color]::FromArgb(0, 10, 20, 45))
$g.FillPath($shadowBrush, $shadow)

# Основа: сине-фиолетовый градиент
$rect = New-RoundedPath 42 42 940 940 216
$grad = New-Object System.Drawing.Drawing2D.LinearGradientBrush(
    (New-Object System.Drawing.Point(42, 60)),
    (New-Object System.Drawing.Point(982, 982)),
    [System.Drawing.Color]::FromArgb(255, 47, 123, 246),
    [System.Drawing.Color]::FromArgb(255, 139, 92, 246)
)
$g.FillPath($grad, $rect)

# Верхний блик (плавно растворяется к низу, без видимой границы)
$glowPath = New-RoundedPath 42 42 940 940 216
$glow = New-Object System.Drawing.Drawing2D.LinearGradientBrush(
    (New-Object System.Drawing.Point(0, 42)),
    (New-Object System.Drawing.Point(0, 900)),
    [System.Drawing.Color]::FromArgb(55, 255, 255, 255),
    [System.Drawing.Color]::FromArgb(0, 255, 255, 255)
)
$g.FillPath($glow, $glowPath)

# Внутренняя обводка
$border = New-RoundedPath 54 54 916 916 206
$borderPen = New-Object System.Drawing.Pen([System.Drawing.Color]::FromArgb(46, 255, 255, 255), 6)
$g.DrawPath($borderPen, $border)

# Шторка: три складки
$pen = New-Object System.Drawing.Pen([System.Drawing.Color]::FromArgb(245, 255, 255, 255), 58)
$pen.StartCap = [System.Drawing.Drawing2D.LineCap]::Round
$pen.EndCap = [System.Drawing.Drawing2D.LineCap]::Round
$pen.LineJoin = [System.Drawing.Drawing2D.LineJoin]::Round

# Левая, центральная и правая складки слегка изгибаются, как ткань.
$left = New-Object System.Drawing.Drawing2D.GraphicsPath
$left.AddBezier(336, 286, 306, 420, 310, 560, 344, 676)
$g.DrawPath($pen, $left)

$mid = New-Object System.Drawing.Drawing2D.GraphicsPath
$mid.AddBezier(512, 250, 500, 400, 524, 540, 512, 676)
$g.DrawPath($pen, $mid)

$right = New-Object System.Drawing.Drawing2D.GraphicsPath
$right.AddBezier(688, 286, 718, 420, 714, 560, 680, 676)
$g.DrawPath($pen, $right)

# Нижний край шторки — волна.
$hem = New-Object System.Drawing.Drawing2D.GraphicsPath
$hem.AddBezier(262, 744, 320, 792, 370, 792, 420, 748)
$hem.AddBezier(420, 748, 470, 706, 490, 706, 512, 748)
$hem.AddBezier(512, 748, 534, 706, 554, 706, 604, 748)
$hem.AddBezier(604, 748, 654, 792, 704, 792, 762, 744)
$hemPen = New-Object System.Drawing.Pen([System.Drawing.Color]::FromArgb(220, 255, 255, 255), 42)
$hemPen.StartCap = [System.Drawing.Drawing2D.LineCap]::Round
$hemPen.EndCap = [System.Drawing.Drawing2D.LineCap]::Round
$hemPen.LineJoin = [System.Drawing.Drawing2D.LineJoin]::Round
$g.DrawPath($hemPen, $hem)

$out = Join-Path (Split-Path -Parent $PSScriptRoot) 'client\icons\icon-source.png'
$bmp.Save($out, [System.Drawing.Imaging.ImageFormat]::Png)

$pen.Dispose(); $hemPen.Dispose(); $borderPen.Dispose()
$grad.Dispose(); $glow.Dispose(); $shadowBrush.Dispose()
$rect.Dispose(); $glowPath.Dispose(); $border.Dispose(); $shadow.Dispose()
$left.Dispose(); $mid.Dispose(); $right.Dispose(); $hem.Dispose()
$g.Dispose(); $bmp.Dispose()
Write-Host "Saved $out"

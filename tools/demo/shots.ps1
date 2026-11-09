# 生成界面截图（用模拟数据，无需真机）：输出到 docs\screenshots
$ErrorActionPreference = 'Continue'
$root = Split-Path (Split-Path $PSScriptRoot)
$work = Join-Path $root 'tmp\demo'; New-Item -ItemType Directory -Force $work | Out-Null
$out = Join-Path $root 'docs\screenshots'; New-Item -ItemType Directory -Force $out | Out-Null
$html = [IO.File]::ReadAllText((Join-Path $root 'web\index.html'))
$mock = [IO.File]::ReadAllText((Join-Path $PSScriptRoot 'mock.js'))
$html = $html.Replace('<head>', "<head>`n<script>`n$mock`n</script>")
[IO.File]::WriteAllText((Join-Path $work 'demo.html'), $html, [Text.UTF8Encoding]::new($false))
$edge = @("${env:ProgramFiles(x86)}\Microsoft\Edge\Application\msedge.exe", "$env:ProgramFiles\Microsoft\Edge\Application\msedge.exe", "$env:ProgramFiles\Google\Chrome\Application\chrome.exe") | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $edge) { throw '需要 Microsoft Edge 或 Google Chrome' }
Add-Type -AssemblyName System.Drawing

function Shot($url, $w, $h, $file, [switch]$Light) {
  $prof = Join-Path $work ("p" + [guid]::NewGuid().ToString('N'))
  $theme = @(); if (-not $Light) { $theme += '--force-dark-mode' }
  & $edge --headless=new --disable-gpu --hide-scrollbars --allow-file-access-from-files --force-device-scale-factor=2 @theme "--user-data-dir=$prof" --virtual-time-budget=5000 "--window-size=$w,$h" "--screenshot=$file" $url 2>&1 | Out-Null
}
function Crop($file, $w) {
  $img = [Drawing.Image]::FromFile($file)
  $bmp = $img.Clone([Drawing.Rectangle]::new(0, 0, [Math]::Min($w, $img.Width), $img.Height), $img.PixelFormat)
  $img.Dispose(); $bmp.Save($file, [Drawing.Imaging.ImageFormat]::Png); $bmp.Dispose()
}
function Mobile($s, $h, $file) {
  $wrap = Join-Path $work "m_$s.html"
  $src = 'file:///' + ((Join-Path $work 'demo.html') -replace '\\', '/') + "?s=$s"
  [IO.File]::WriteAllText($wrap, "<html><body style='margin:0;background:#000'><iframe src='$src' style='width:390px;height:${h}px;border:0;display:block'></iframe></body></html>")
  Shot ('file:///' + ($wrap -replace '\\', '/')) 600 $h $file
  Crop $file 780
}

Get-ChildItem $out -Filter *.png | Remove-Item
$demo = 'file:///' + ((Join-Path $work 'demo.html') -replace '\\', '/')
Shot "$demo`?s=desk" 1440 900 (Join-Path $out '1-desktop-dark.png')
Shot "$demo`?s=desk" 1440 900 (Join-Path $out '2-desktop-light.png') -Light
Shot "$demo`?s=add" 1280 760 (Join-Path $out '3-add-computer.png')
Shot "$demo`?s=menu" 1280 900 (Join-Path $out '4-device-menu.png')
Shot "$demo`?s=settings" 1280 900 (Join-Path $out '5-settings.png')
Shot "$demo`?s=log" 1440 900 (Join-Path $out '8-logcat.png')
Mobile 'mlist' 844 (Join-Path $out '6-mobile-list.png')
Mobile 'mview' 844 (Join-Path $out '7-mobile-view.png')
Remove-Item -Recurse -Force (Join-Path $root 'tmp') -ErrorAction SilentlyContinue
Get-ChildItem $out | Select-Object Name, Length

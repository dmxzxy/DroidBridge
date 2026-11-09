# 下载打包所需的第三方组件（不纳入 Git）：
#   third_party/platform-tools   Google Android SDK Platform-Tools（adb），Windows 安装包随附
# 并校验仓库内置的 scrcpy-server 与官方发布一致。
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot
$tp = Join-Path $root 'third_party'

$version = (Get-Content (Join-Path $tp 'scrcpy-version.txt') -Raw).Trim()
$expected = (Get-Content (Join-Path $tp 'scrcpy-server.sha256') -Raw).Trim().Split(' ')[0]
$actual = (Get-FileHash (Join-Path $tp 'scrcpy-server') -Algorithm SHA256).Hash.ToLower()
if ($actual -ne $expected) { throw "scrcpy-server 校验失败：$actual，应为 $expected（scrcpy v$version）" }
Write-Host "scrcpy-server v$version 校验通过"

$pt = Join-Path $tp 'platform-tools'
if (Test-Path (Join-Path $pt 'adb.exe')) {
  Write-Host 'platform-tools 已存在，跳过下载'
} else {
  Write-Host '下载 Android SDK Platform-Tools…'
  $zip = Join-Path $env:TEMP 'platform-tools-latest-windows.zip'
  Invoke-WebRequest https://dl.google.com/android/repository/platform-tools-latest-windows.zip -OutFile $zip -UseBasicParsing
  Expand-Archive $zip -DestinationPath $tp -Force
  Remove-Item $zip
  Write-Host "已解压到 $pt"
}

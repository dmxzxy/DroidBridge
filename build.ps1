# 打包：生成 dist\DroidBridge（桌面版 + 服务版 + platform-tools），可直接拷到其他电脑使用
#   powershell -File build.ps1 [-Version 0.2.0]
param([string]$Version = '')
$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot
& (Join-Path $PSScriptRoot 'tools\fetch-deps.ps1')

# 去掉编译进程序里的本机路径（依赖库源码位置含用户名）
$cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $env:USERPROFILE '.cargo' }
$env:CARGO_ENCODED_RUSTFLAGS = @("--remap-path-prefix=$cargoHome=/cargo", "--remap-path-prefix=$PSScriptRoot=/droidbridge") -join [char]0x1f
cargo build --release --features desktop
if ($LASTEXITCODE -ne 0) { throw 'cargo build 失败' }
Remove-Item Env:CARGO_ENCODED_RUSTFLAGS

$dist = Join-Path $PSScriptRoot 'dist\DroidBridge'
New-Item -ItemType Directory -Force $dist | Out-Null
# 保留运行数据（data：远程电脑列表、证书等），只替换程序文件
Get-ChildItem $dist | Where-Object { $_.Name -ne 'data' } | Remove-Item -Recurse -Force
Copy-Item target\release\droidbridge-desktop.exe (Join-Path $dist 'DroidBridge.exe')
Copy-Item target\release\droidbridge.exe (Join-Path $dist 'droidbridge-server.exe')
Copy-Item third_party\platform-tools (Join-Path $dist 'platform-tools') -Recurse
Copy-Item start-server.bat, README.md, LICENSE $dist
Copy-Item third_party\scrcpy-LICENSE (Join-Path $dist 'scrcpy-LICENSE.txt')

$zipName = if ($Version) { "DroidBridge-$Version-windows-x64.zip" } else { 'DroidBridge-win64.zip' }
$zip = Join-Path $PSScriptRoot "dist\$zipName"
if (Test-Path $zip) { Remove-Item $zip }
Compress-Archive -Path (Get-ChildItem $dist | Where-Object { $_.Name -ne 'data' }).FullName -DestinationPath $zip
Write-Host "完成：$dist"
Write-Host "压缩包：$zip"

param([Parameter(Mandatory = $true)][string]$Version)
# 构建 Windows 安装包：tools\make-installer.ps1 -Version 0.7.0
# 需要 Inno Setup 6（ISCC.exe）：winget install -e --id JRSoftware.InnoSetup
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot
Push-Location $root
try {
  $iscc = @(
    "$env:ProgramFiles\Inno Setup 6\ISCC.exe",
    "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
    "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe"
  ) | Where-Object { Test-Path $_ } | Select-Object -First 1
  if (-not $iscc) { throw '未找到 ISCC.exe，请先安装 Inno Setup 6：winget install -e --id JRSoftware.InnoSetup' }

  if (-not (Test-Path 'dist\DroidBridge\DroidBridge.exe')) { throw '请先运行 build.ps1 生成 dist\DroidBridge' }

  cargo run -q --release --example icons -- 'target\icons'
  if ($LASTEXITCODE -ne 0) { throw '生成图标失败' }

  # 版本号通过环境变量传给 installer.iss
  $env:DB_VERSION = $Version
  & $iscc 'tools\installer.iss'
  if ($LASTEXITCODE -ne 0) { throw 'Inno Setup 编译失败' }

  $out = "dist\DroidBridge-$Version-windows-x64-setup.exe"
  if (Test-Path $out) { Write-Host "安装包：$out" } else { throw '没有生成安装包' }
}
finally { Pop-Location }

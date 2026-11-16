# 插件宿主集成自测（PowerShell）：
#   先 cargo build，然后 powershell -ExecutionPolicy Bypass -File tools/plugins/host-selftest.ps1
# 启动 target/debug/droidbridge --plugin-host，喂 init + req menu，校验 NDJSON 输出。

$ErrorActionPreference = 'Stop'
Set-Location (Join-Path $PSScriptRoot '..\..')

$exe = 'target\debug\droidbridge.exe'
if (-not (Test-Path $exe)) { $exe = 'target\debug\droidbridge' }
if (-not (Test-Path $exe)) { throw "缺少 $exe，先执行 cargo build" }

$root = (Get-Location).Path
$work = Join-Path $root 'target\plugin-host-selftest'
if (Test-Path $work) { Remove-Item $work -Recurse -Force }
New-Item -ItemType Directory -Force (Join-Path $work 'plugin') | Out-Null
New-Item -ItemType Directory -Force (Join-Path $work 'data') | Out-Null

$lua = @'
db.menu("tools", "Selftest", function()
  print("selftest menu ran")
  return "selftest-ok"
end)
print("selftest boot")
'@
# 无 BOM 的 UTF-8
[System.IO.File]::WriteAllText((Join-Path $work 'plugin\main.lua'), $lua, (New-Object System.Text.UTF8Encoding($false)))

$dir = (Join-Path $work 'plugin') -replace '\\', '/'
$data = (Join-Path $work 'data') -replace '\\', '/'
$out = New-Object System.Collections.Generic.List[string]

$psi = New-Object System.Diagnostics.ProcessStartInfo
$psi.FileName = (Join-Path $root $exe)
$psi.Arguments = '--plugin-host'
$psi.RedirectStandardInput = $true
$psi.RedirectStandardOutput = $true
$psi.RedirectStandardError = $true
$psi.UseShellExecute = $false
$psi.StandardOutputEncoding = [System.Text.Encoding]::UTF8
$psi.StandardErrorEncoding = [System.Text.Encoding]::UTF8
# 刚 build 完时 exe 可能被短暂锁住，重试几次
$proc = $null
for ($i = 0; $i -lt 3 -and -not $proc; $i++) {
  try { $proc = [System.Diagnostics.Process]::Start($psi) }
  catch { Start-Sleep -Milliseconds 800 }
}
if (-not $proc) { throw '无法启动插件宿主进程' }

function Send-Line([string]$text) {
  $proc.StandardInput.WriteLine($text)
  $proc.StandardInput.Flush()
}

function Read-Line {
  $task = $proc.StandardOutput.ReadLineAsync()
  if (-not $task.Wait(10000)) { throw '读取宿主输出超时' }
  return $task.Result
}

function Read-Until([scriptblock]$match) {
  for ($i = 0; $i -lt 60; $i++) {
    $line = Read-Line
    if ($null -eq $line) { throw '宿主提前退出' }
    $out.Add($line)
    if ($line -match '"t":"call"') {
      $m = [regex]::Match($line, '"id":(\d+)')
      if ($m.Success) { Send-Line ('{"t":"ret","id":' + $m.Groups[1].Value + ',"ok":true}') }
    }
    if (& $match $line) { return $line }
  }
  throw '等待超时'
}

$init = '{"t":"init","plugin":{"id":"selftest","name":"Selftest","version":"1.0.0","dir":"' + $dir +
  '","dataDir":"' + $data + '","main":"main.lua"},"granted":[],"settings":{},"lang":"zh","node":"pc"}'
Send-Line $init
Read-Until { param($l) $l -match '"t":"ready"' } | Out-Null

Send-Line '{"t":"req","id":1,"m":"menu","p":{"id":"tools:Selftest","ctx":{}}}'
$res = Read-Until { param($l) ($l -match '"t":"res"') -and ($l -match '"id":1,') }

$proc.StandardInput.Close()
$proc.WaitForExit(5000) | Out-Null
if (-not $proc.HasExited) { $proc.Kill() }
$err = $proc.StandardError.ReadToEnd()

$text = ($out -join "`n")
$ok = $true
if ($text -notmatch '"t":"ready"') { Write-Host '缺少 ready'; $ok = $false }
if ($text -notmatch '"m":"register"') { Write-Host '缺少 register'; $ok = $false }
if ($text -notmatch '"kind":"menu"') { Write-Host '缺少 register menu'; $ok = $false }
if ($text -notmatch 'selftest boot') { Write-Host '缺少 boot 日志'; $ok = $false }
if ($text -notmatch 'selftest menu ran') { Write-Host '缺少菜单日志'; $ok = $false }
if ($res -notmatch '"ok":"selftest-ok"') { Write-Host "res 内容不对: $res"; $ok = $false }

if (-not $ok) {
  Write-Host '---- host output ----'
  Write-Host $text
  Write-Host '---- host stderr ----'
  Write-Host $err
  exit 1
}
Write-Host 'SELFTEST OK'
Write-Host '---- host output ----'
Write-Host $text

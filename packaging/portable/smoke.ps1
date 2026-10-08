# Smoke-tests the portable Windows zip on this (throwaway) Windows machine as a person would use
# it: unzipped into a fresh folder outside the checkout, never installed. The release workflow's
# portable job runs it; see packaging/README.md, "The portable Windows zip".
#
#   pwsh -File packaging/portable/smoke.ps1 -Zip <pitcrew-windows-x64-portable.zip> [-Channel release|main]
#
# Checks:
# - the zip holds exactly the expected files, helpers/ included, and they match its SHA256SUMS,
#   also with the PowerShell lines README-portable.txt gives; portable.txt is on -Channel;
# - no program imports the Visual C++ runtime (VCRUNTIME140.dll and the like), which this runner
#   has but a machine that cannot run installers may lack, and pitcrewd and the app use Windows'
#   own UCRT, which Windows Update keeps current: read from each program's PE headers;
# - pitcrewd, pitcrew and pitcrew-ptyd answer --version with one version; pitcrew-askpass refuses
#   to run outside ssh;
# - helpers/: the Linux helpers, decoded, match manifest.json, which is pitcrewd's version and
#   exactly the manifest compiled into pitcrew-desktop.exe;
# - pitcrew-desktop.exe --check-layout (no window) finds its four programs, portable.txt on its
#   channel and both Linux helpers (the app's own lookup, with its compiled checksums), with the
#   state directory outside the folder, also through `| Out-String` as README-portable.txt says,
#   and refuses a pitcrewd.exe still marked as downloaded (Zone.Identifier) until it is unblocked;
# - pitcrewd.exe, with a temporary state directory and the demo workspace (so its runner starts,
#   watching no agent home), answers GET /v1/host/info on a loopback port, as pitcrewd, with the
#   runner's terminals in the pitcrew-ptyd next to it (capability `pty`);
# - nothing was written into the unzipped folder.
# On GitHub Actions it adds the files, their sizes and the zip's SHA-256 to the job's summary.
param(
  [Parameter(Mandatory = $true)] [string] $Zip,
  [ValidateSet('release', 'main')] [string] $Channel = 'main'
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version 3

$script:failures = 0
function Fail([string] $message) { Write-Host "::error::portable: $message"; $script:failures++ }
function Ok([string] $message) { Write-Host "  ok: portable: $message" }

# Runs a program to the end; its exit code and what it wrote (stdout then stderr).
function Invoke-Program([string] $program, [string[]] $arguments) {
  $out = Join-Path $env:TEMP ('pitcrew-portable-' + [guid]::NewGuid().ToString('N'))
  $p = Start-Process -FilePath $program -ArgumentList $arguments -NoNewWindow -Wait -PassThru `
    -RedirectStandardOutput "$out.out" -RedirectStandardError "$out.err"
  $text = "$(Get-Content -Raw "$out.out")$(Get-Content -Raw "$out.err")"
  Remove-Item "$out.out", "$out.err" -ErrorAction SilentlyContinue
  return [pscustomobject]@{ Code = $p.ExitCode; Text = $text.Trim() }
}

# The DLLs a PE file imports, directly or delay-loaded, from its import directories.
function Get-Imports([string] $path) {
  $b = [IO.File]::ReadAllBytes($path)
  $pe = [BitConverter]::ToInt32($b, 0x3C)
  if ([Text.Encoding]::ASCII.GetString($b, $pe, 4) -ne "PE`0`0") { throw "$path is not a PE file" }
  $sections = [BitConverter]::ToUInt16($b, $pe + 6)
  $optional = $pe + 24
  $table = $optional + [BitConverter]::ToUInt16($b, $pe + 20)
  $directories = if ([BitConverter]::ToUInt16($b, $optional) -eq 0x20b) { $optional + 112 } else { $optional + 96 }
  $offset = {
    param([long] $rva)
    for ($i = 0; $i -lt $sections; $i++) {
      $s = $table + 40 * $i
      $va = [long][BitConverter]::ToUInt32($b, $s + 12)
      $size = [Math]::Max([long][BitConverter]::ToUInt32($b, $s + 8), [long][BitConverter]::ToUInt32($b, $s + 16))
      if ($rva -ge $va -and $rva -lt $va + $size) { return [int]($rva - $va + [BitConverter]::ToUInt32($b, $s + 20)) }
    }
    throw "$path`: RVA $rva is in no section"
  }
  $names = [Collections.Generic.List[string]]::new()
  # Directory 1: imports (20-byte descriptors, the name's RVA at 12); directory 13: delay-loaded
  # imports (32-byte descriptors, the name's RVA at 4).
  foreach ($d in @(@{ Index = 1; Size = 20; Name = 12 }, @{ Index = 13; Size = 32; Name = 4 })) {
    $rva = [BitConverter]::ToUInt32($b, $directories + 8 * $d.Index)
    if ($rva -eq 0) { continue }
    $at = & $offset $rva
    while ($true) {
      $nameRva = [BitConverter]::ToUInt32($b, $at + $d.Name)
      if ($nameRva -eq 0) { break }
      $o = & $offset $nameRva
      $end = [Array]::IndexOf($b, [byte]0, $o)
      $names.Add([Text.Encoding]::ASCII.GetString($b, $o, $end - $o))
      $at += $d.Size
    }
  }
  return $names
}

$Zip = (Resolve-Path -LiteralPath $Zip).Path
$root = Join-Path ([IO.Path]::GetTempPath()) ('pitcrew-portable-' + [guid]::NewGuid().ToString('N'))
$dir = Join-Path $root 'PitCrew'
New-Item -ItemType Directory -Path $dir | Out-Null
Expand-Archive -LiteralPath $Zip -DestinationPath $dir
Ok "unzipped into $dir"

# --- The files, and SHA256SUMS
$programs = @('pitcrew-desktop.exe', 'pitcrewd.exe', 'pitcrew-ptyd.exe', 'pitcrew-askpass.exe', 'pitcrew.exe')
$helpers = @('pitcrewd-aarch64-unknown-linux-musl', 'pitcrewd-x86_64-unknown-linux-musl')
$expected = @($programs + @('LICENSE', 'NOTICE', 'THIRD-PARTY-NOTICES.txt', 'README-portable.txt', 'portable.txt', 'SHA256SUMS', 'helpers/manifest.json') + @($helpers | ForEach-Object { "helpers/$_.xz" }))
# Every file in the folder, by its path there, with / as in SHA256SUMS.
function Get-Names {
  @(Get-ChildItem -LiteralPath $dir -Recurse -File -Force | ForEach-Object { [IO.Path]::GetRelativePath($dir, $_.FullName).Replace('\', '/') })
}
$names = @(Get-Names)
if (Compare-Object -ReferenceObject $expected -DifferenceObject $names -CaseSensitive) {
  Fail "the zip holds $($names -join ', '), not $($expected -join ', ')"
} else {
  Ok "the zip holds exactly $($expected.Count) files: $($names -join ', ')"
}

$listed = @{}
foreach ($line in Get-Content -LiteralPath (Join-Path $dir 'SHA256SUMS')) {
  if ($line -cmatch '^([0-9a-f]{64})  (\S.*)$') { $listed[$Matches[2]] = $Matches[1] } else { Fail "SHA256SUMS: cannot read '$line'" }
}
foreach ($name in $names) {
  if ($name -eq 'SHA256SUMS') { continue }
  if (-not $listed.ContainsKey($name)) { Fail "$name is not in SHA256SUMS"; continue }
  $hash = (Get-FileHash -Algorithm SHA256 -LiteralPath (Join-Path $dir $name)).Hash.ToLowerInvariant()
  if ($hash -ceq $listed[$name]) { Ok "$name matches SHA256SUMS" } else { Fail "$name does not match SHA256SUMS" }
}
if ($listed.Count -ne $names.Count - 1) { Fail "SHA256SUMS lists $($listed.Count) files, not $($names.Count - 1)" }
if ([IO.File]::ReadAllText((Join-Path $dir 'portable.txt')) -cmatch "(?m)^channel=$Channel\r?$") { Ok "portable.txt says channel=$Channel" } else { Fail "portable.txt does not say channel=$Channel" }

# README-portable.txt's own check, as written there, run in the folder.
$readme = @(Get-Content -LiteralPath (Join-Path $dir 'README-portable.txt'))
$first = -1
$last = -1
for ($i = 0; $i -lt $readme.Count; $i++) {
  if ($first -lt 0 -and $readme[$i].Trim().StartsWith('Get-Content SHA256SUMS')) { $first = $i }
  elseif ($first -ge 0 -and $readme[$i].Trim() -eq '}') { $last = $i; break }
}
if ($first -lt 0 -or $last -lt 0) {
  Fail "README-portable.txt has no SHA256SUMS check to run"
} else {
  Push-Location -LiteralPath $dir
  try { $lines = @(Invoke-Expression ($readme[$first..$last] -join "`n")) } finally { Pop-Location }
  $good = @($lines | Where-Object { $_ -like 'ok   *' })
  if ($good.Count -eq $listed.Count -and $good.Count -eq $lines.Count) { Ok "README-portable.txt's check says ok for all $($good.Count) files" } else { Fail "README-portable.txt's check said: $($lines -join '; ')" }
}

# --- No Visual C++ runtime needed
foreach ($name in $programs) {
  $imports = @(Get-Imports (Join-Path $dir $name))
  Write-Host "  $name imports: $($imports -join ', ')"
  $runtime = @($imports | Where-Object { $_ -match '^(vcruntime|msvcp|vccorlib|concrt|vcomp)\d|^ucrtbased\.dll$' })
  if ($runtime.Count -gt 0) { Fail "$name needs the Visual C++ runtime: $($runtime -join ', ')" } else { Ok "$name needs no Visual C++ runtime" }
  # The UCRT stays Windows' own (dynamic), as tauri-build leaves it for the app.
  if ($name -in @('pitcrew-desktop.exe', 'pitcrewd.exe')) {
    if (@($imports | Where-Object { $_ -match '^(api-ms-win-crt-|ucrtbase\.dll$)' }).Count -gt 0) { Ok "$name uses Windows' own UCRT" } else { Fail "$name does not import the UCRT: is it linked in statically?" }
  }
}

# --- The programs run
$r = Invoke-Program (Join-Path $dir 'pitcrewd.exe') @('--version')
$version = if ($r.Code -eq 0 -and $r.Text -match '^pitcrewd (\S+) \(protocol') { $Matches[1] } else { $null }
if ($version) { Ok $r.Text } else { Fail "pitcrewd --version: exit $($r.Code): $($r.Text)" }
$r = Invoke-Program (Join-Path $dir 'pitcrew.exe') @('--version')
if ($r.Code -eq 0 -and $r.Text -ceq "pitcrew $version") { Ok $r.Text } else { Fail "pitcrew --version: exit $($r.Code): $($r.Text)" }
$r = Invoke-Program (Join-Path $dir 'pitcrew-ptyd.exe') @('--version')
if ($r.Code -eq 0 -and $r.Text -like "pitcrew-ptyd $version *") { Ok $r.Text } else { Fail "pitcrew-ptyd --version: exit $($r.Code): $($r.Text)" }
Remove-Item Env:PITCREW_ASKPASS_ADDR -ErrorAction SilentlyContinue
$r = Invoke-Program (Join-Path $dir 'pitcrew-askpass.exe') @('Password:')
if ($r.Code -eq 2 -and $r.Text -like '*not started by PitCrew*') { Ok 'pitcrew-askpass runs (and refuses to answer outside ssh)' } else { Fail "pitcrew-askpass: exit $($r.Code): $($r.Text)" }

# --- The remote helpers: each decodes to the bytes manifest.json names, and that manifest is the
# one compiled into the app, the only checksums it trusts before deploying a helper
$desktop = Join-Path $dir 'pitcrew-desktop.exe'
$manifestText = [IO.File]::ReadAllText((Join-Path $dir 'helpers/manifest.json'))
$manifest = $manifestText | ConvertFrom-Json
$listedHelpers = @($manifest.sha256.PSObject.Properties | ForEach-Object { $_.Name } | Sort-Object)
if (($listedHelpers -join ',') -ceq ($helpers -join ',')) { Ok "helpers/manifest.json lists $($helpers -join ' and ')" } else { Fail "helpers/manifest.json lists $($listedHelpers -join ', ')" }
if ($manifest.version -ceq $version) { Ok "the helpers are version $version, pitcrewd's" } else { Fail "the helpers' version is $($manifest.version), not pitcrewd's $version" }
# Git's bash, with xz, as the release workflow's Windows checks use it.
$gitBash = if ($env:ProgramFiles) { Join-Path $env:ProgramFiles 'Git\bin\bash.exe' } else { '' }
$bash = if ($gitBash -and (Test-Path -LiteralPath $gitBash)) { $gitBash } else { 'bash' }
foreach ($entry in $manifest.sha256.PSObject.Properties) {
  $decoded = Join-Path $root "$($entry.Name).decoded"
  $env:PITCREW_CHECK_XZ = Join-Path $dir "helpers/$($entry.Name).xz"
  $env:PITCREW_CHECK_DECODED = $decoded
  & $bash -c 'xz -dc -- "$PITCREW_CHECK_XZ" > "$PITCREW_CHECK_DECODED"'
  if ($LASTEXITCODE -ne 0 -or -not (Test-Path -LiteralPath $decoded)) { Fail "helpers/$($entry.Name).xz does not decode"; continue }
  $hash = (Get-FileHash -Algorithm SHA256 -LiteralPath $decoded).Hash.ToLowerInvariant()
  $size = (Get-Item -LiteralPath $decoded).Length
  Remove-Item -LiteralPath $decoded -Force
  if ($hash -ceq $entry.Value) { Ok "helpers/$($entry.Name).xz decodes to its manifest's sha256 ($size bytes)" } else { Fail "helpers/$($entry.Name).xz does not decode to its manifest's sha256" }
}
Remove-Item Env:PITCREW_CHECK_XZ, Env:PITCREW_CHECK_DECODED -ErrorAction SilentlyContinue
$latin1 = [Text.Encoding]::GetEncoding(28591)
if ($latin1.GetString([IO.File]::ReadAllBytes($desktop)).Contains($manifestText)) { Ok 'helpers/manifest.json is compiled into pitcrew-desktop.exe' } else { Fail 'pitcrew-desktop.exe does not hold helpers/manifest.json (PITCREW_HELPERS_MANIFEST)' }

# --- The desktop's own view of its folder, with no window
$portableLine = if ($Channel -eq 'release') { 'portable: yes, release channel' } else { 'portable: yes, development build' }
$defaultState = Join-Path $env:LOCALAPPDATA 'PitCrew\data'
function Test-Layout([string] $text, [string] $how) {
  $wanted = @($portableLine, 'layout: ok', "state: $defaultState") + @($programs | Select-Object -Skip 1 | ForEach-Object { "ok: $_" }) + @($helpers | ForEach-Object { "helper ${_}: ok (" })
  $missing = @($wanted | Where-Object { $text -notmatch "(?m)^$([regex]::Escape($_))" })
  if ($text -notmatch '(?m)^WebView2: \d') { $missing += 'WebView2: <version>' }
  # Where the state is, which must not be the unzipped folder.
  if ($text -match '(?m)^state: (.+?)\r?$' -and $Matches[1].StartsWith($dir, [StringComparison]::OrdinalIgnoreCase)) { $missing += "a state directory outside $dir" }
  if ($missing.Count -eq 0) { Ok "pitcrew-desktop --check-layout ($how) finds everything, and the state in $defaultState" } else { Fail "pitcrew-desktop --check-layout ($how) lacks: $($missing -join '; ')" }
}
$r = Invoke-Program $desktop @('--check-layout')
Write-Host $r.Text
if ($r.Code -ne 0) { Fail "pitcrew-desktop --check-layout exited with $($r.Code)" }
Test-Layout $r.Text 'redirected'
$text = & $desktop --check-layout | Out-String
Test-Layout $text 'piped to Out-String, as README-portable.txt says'

$pitcrewd = Join-Path $dir 'pitcrewd.exe'
Set-Content -LiteralPath $pitcrewd -Stream Zone.Identifier -Value "[ZoneTransfer]`r`nZoneId=3"
$r = Invoke-Program $desktop @('--check-layout')
if ($r.Code -eq 1 -and $r.Text -match '(?m)^not usable: pitcrewd\.exe: .*Zone\.Identifier') { Ok 'a pitcrewd.exe marked as downloaded is refused, and the reason given' } else { Fail "a pitcrewd.exe marked as downloaded: exit $($r.Code): $($r.Text)" }
Unblock-File -LiteralPath $pitcrewd
$r = Invoke-Program $desktop @('--check-layout')
if ($r.Code -eq 0) { Ok 'unblocked, it is used again' } else { Fail "unblocked: exit $($r.Code): $($r.Text)" }

# --- pitcrewd serves, with its runner's terminals in the pitcrew-ptyd next to it
$state = Join-Path $root 'state'
$listener = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0)
$listener.Start()
$port = $listener.LocalEndpoint.Port
$listener.Stop()
$log = Join-Path $root 'pitcrewd'
$env:PITCREW_LOG = 'info'
$daemon = Start-Process -FilePath $pitcrewd -PassThru -NoNewWindow `
  -RedirectStandardOutput "$log.out" -RedirectStandardError "$log.err" `
  -ArgumentList @('--state-dir', "`"$state`"", 'serve', '--demo', '--listen', "tcp:127.0.0.1:$port")
$ready = $false
for ($i = 0; $i -lt 60 -and -not $daemon.HasExited; $i++) {
  Start-Sleep -Seconds 1
  if ("$(Get-Content -Raw "$log.out" -ErrorAction SilentlyContinue)" -like '*pitcrewd listening on*') { $ready = $true; break }
}
$info = $null
if ($ready) {
  Ok "pitcrewd is ready on 127.0.0.1:$port with its state in $state"
  for ($i = 0; $i -lt 30; $i++) {
    try { $info = Invoke-RestMethod -Uri "http://127.0.0.1:$port/v1/host/info" -TimeoutSec 5 } catch { $info = $null }
    if ($info -and @($info.roles) -contains 'runner') { break }
    Start-Sleep -Seconds 1
  }
} else {
  Fail 'pitcrewd did not say it was listening within 60 s'
}
if ($info) {
  Write-Host "  GET /v1/host/info: $($info | ConvertTo-Json -Compress -Depth 5)"
  if ($info.name -eq 'pitcrewd' -and $info.version -eq $version) { Ok "GET /v1/host/info answers: pitcrewd $($info.version), protocol $($info.protocol)" } else { Fail "GET /v1/host/info is not this pitcrewd's" }
  if (@($info.roles) -contains 'runner') { Ok 'its runner runs' } else { Fail "its roles are $(@($info.roles) -join ', '), without runner" }
  if (@($info.capabilities) -contains 'pty') { Ok "its terminals run in pitcrew-ptyd, found next to pitcrewd.exe" } else { Fail "its capabilities are $(@($info.capabilities) -join ', '), without pty" }
} elseif ($ready) {
  Fail 'GET /v1/host/info did not answer'
}
if (-not $daemon.HasExited) { Stop-Process -Id $daemon.Id -Force }
Get-Process -Name pitcrew-ptyd -ErrorAction SilentlyContinue | Where-Object { $_.Path -like "$dir\*" } | Stop-Process -Force
Start-Sleep -Seconds 2
Write-Host '--- pitcrewd''s log'
Write-Host "$(Get-Content -Raw "$log.out" -ErrorAction SilentlyContinue)$(Get-Content -Raw "$log.err" -ErrorAction SilentlyContinue)"
Write-Host '---'
Remove-Item Env:PITCREW_LOG

# --- Nothing written next to the programs
$after = @(Get-Names)
if (Compare-Object -ReferenceObject $expected -DifferenceObject $after -CaseSensitive) { Fail "the folder now holds $($after -join ', ')" } else { Ok 'nothing was written into the unzipped folder' }

# --- The summary
$zipHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $Zip).Hash.ToLowerInvariant()
$zipSize = (Get-Item -LiteralPath $Zip).Length
$rows = foreach ($name in ($names | Sort-Object)) { "| ``$name`` | $((Get-Item -LiteralPath (Join-Path $dir $name)).Length) |" }
$summary = @(
  "## pitcrew-windows-x64-portable.zip",
  '',
  "PitCrew $version (channel=$Channel), $zipSize bytes, SHA-256 ``$zipHash``.",
  '',
  '| File | Bytes |',
  '|---|---:|'
) + $rows + @('', "Smoke test: $(if ($script:failures -eq 0) { 'every check passed' } else { "$($script:failures) check(s) failed" }).")
Write-Host ($summary -join "`n")
if ($env:GITHUB_STEP_SUMMARY) { Add-Content -LiteralPath $env:GITHUB_STEP_SUMMARY -Value ($summary -join "`n") }

Remove-Item -Recurse -Force $root -ErrorAction SilentlyContinue
if ($script:failures -gt 0) {
  Write-Host "portable: $($script:failures) check(s) failed"
  exit 1
}
Write-Host 'portable: every check passed'

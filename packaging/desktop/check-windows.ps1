# Installs a PitCrew NSIS installer on this (throwaway) Windows machine as a person would (for
# this user, silently), checks what it installed and registered, starts the app once, installs
# again over a running pitcrew-ptyd, then removes it. packaging/desktop/check.sh calls it.
#
#   powershell -File packaging/desktop/check-windows.ps1 -Installer <…-setup.exe> -Manifest <manifest.json>
#
# Checks:
# - pitcrew-desktop.exe, pitcrewd.exe, pitcrew-ptyd.exe and pitcrew-askpass.exe side by side,
#   none with a Zone.Identifier (the app's own trust check refuses one);
# - helpers\ with the three helpers and manifest.json, which is -Manifest, each helper matching
#   its sha256, and -Manifest compiled into pitcrew-desktop.exe;
# - pitcrewd and pitcrew-ptyd answer --version, pitcrew-askpass refuses to run outside ssh;
# - pitcrew:// registered for this user (HKCU\Software\Classes\pitcrew) to open the app;
# - the Start menu shortcut carries the AppUserModelID org.pitcrew.desktop, which the app's
#   notifications are sent as, and Windows lists that app id;
# - the app starts with its own pitcrewd and askpass (its log, in a throwaway state directory);
# - installing again while pitcrew-ptyd runs succeeds, and ptyd keeps running (moved aside by
#   installer-hooks.nsh);
# - removing it unregisters pitcrew:// and removes the programs.
param(
  [Parameter(Mandatory = $true)] [string] $Installer,
  [Parameter(Mandatory = $true)] [string] $Manifest
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version 3

$script:failures = 0
function Fail([string] $message) { Write-Host "::error::NSIS: $message"; $script:failures++ }
function Ok([string] $message) { Write-Host "  ok: NSIS: $message" }

$identifier = 'org.pitcrew.desktop'
$product = 'PitCrew'
$appExe = 'pitcrew-desktop.exe'
$sidecars = @('pitcrewd.exe', 'pitcrew-ptyd.exe', 'pitcrew-askpass.exe')
$expected = [IO.File]::ReadAllText($Manifest)
$manifestJson = $expected | ConvertFrom-Json

# Runs a program to the end; its exit code and what it wrote (stdout then stderr).
function Invoke-Program([string] $program, [string[]] $arguments) {
  $out = Join-Path $env:TEMP ('pitcrew-check-' + [guid]::NewGuid().ToString('N'))
  $p = Start-Process -FilePath $program -ArgumentList $arguments -NoNewWindow -Wait -PassThru `
    -RedirectStandardOutput "$out.out" -RedirectStandardError "$out.err"
  $text = "$(Get-Content -Raw "$out.out")$(Get-Content -Raw "$out.err")"
  Remove-Item "$out.out", "$out.err" -ErrorAction SilentlyContinue
  return [pscustomobject]@{ Code = $p.ExitCode; Text = $text.Trim() }
}

function Install-PitCrew() {
  $p = Start-Process -FilePath $Installer -ArgumentList '/S' -Wait -PassThru
  return $p.ExitCode
}

# --- Install
$code = Install-PitCrew
if ($code -ne 0) { Fail "the installer exited with $code"; exit 1 }
$uninstallKey = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\$product"
$dir = (Get-ItemProperty $uninstallKey).InstallLocation.Trim('"')
Ok "installed for this user in $dir"

# --- Files next to the app
foreach ($name in @($appExe) + $sidecars) {
  $path = Join-Path $dir $name
  if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { Fail "$name is not next to $appExe in $dir"; continue }
  Ok "$name next to the app"
  if (Get-Item -LiteralPath $path -Stream Zone.Identifier -ErrorAction SilentlyContinue) {
    Fail "$name has a Zone.Identifier: the app would refuse it"
  }
}

# --- Helpers and their checksums
$helpers = Join-Path $dir 'helpers'
$installedManifest = Join-Path $helpers 'manifest.json'
if ((Test-Path -LiteralPath $installedManifest) -and ([IO.File]::ReadAllText($installedManifest) -ceq $expected)) {
  Ok 'helpers\manifest.json is the compiled manifest'
} else {
  Fail 'helpers\manifest.json is missing or differs from the compiled manifest'
}
$listed = @($manifestJson.sha256.PSObject.Properties | ForEach-Object { $_.Name })
foreach ($entry in $manifestJson.sha256.PSObject.Properties) {
  $file = Join-Path $helpers $entry.Name
  if (-not (Test-Path -LiteralPath $file)) { Fail "helpers\$($entry.Name) is missing"; continue }
  $hash = (Get-FileHash -Algorithm SHA256 -LiteralPath $file).Hash.ToLowerInvariant()
  if ($hash -eq $entry.Value) { Ok "helpers\$($entry.Name) matches its sha256" } else { Fail "helpers\$($entry.Name) does not match the manifest's sha256" }
}
foreach ($file in Get-ChildItem -LiteralPath $helpers -File) {
  if ($file.Name -ne 'manifest.json' -and $listed -notcontains $file.Name) { Fail "helpers\$($file.Name) is not in the manifest" }
}
$latin1 = [Text.Encoding]::GetEncoding(28591)
if ($latin1.GetString([IO.File]::ReadAllBytes((Join-Path $dir $appExe))).Contains($expected)) {
  Ok "the helpers' checksums are compiled into $appExe"
} else {
  Fail "$appExe does not hold the manifest (built without PITCREW_HELPERS_MANIFEST?)"
}

# --- The sidecars run
$version = $manifestJson.version
$r = Invoke-Program (Join-Path $dir 'pitcrewd.exe') @('--version')
if ($r.Code -eq 0 -and $r.Text -like "pitcrewd $version *") { Ok $r.Text } else { Fail "pitcrewd --version: exit $($r.Code): $($r.Text)" }
$r = Invoke-Program (Join-Path $dir 'pitcrew-ptyd.exe') @('--version')
if ($r.Code -eq 0 -and $r.Text -like "pitcrew-ptyd $version*") { Ok $r.Text } else { Fail "pitcrew-ptyd --version: exit $($r.Code): $($r.Text)" }
Remove-Item Env:PITCREW_ASKPASS_ADDR -ErrorAction SilentlyContinue
$r = Invoke-Program (Join-Path $dir 'pitcrew-askpass.exe') @('Password:')
if ($r.Code -eq 2 -and $r.Text -like '*not started by PitCrew*') { Ok 'pitcrew-askpass runs (and refuses to answer outside ssh)' } else { Fail "pitcrew-askpass: exit $($r.Code): $($r.Text)" }

# --- pitcrew:// links
$classes = 'HKCU:\Software\Classes\pitcrew'
$wantCommand = '"' + (Join-Path $dir $appExe) + '" "%1"'
if (Test-Path $classes) {
  if ((Get-ItemProperty $classes).PSObject.Properties.Name -contains 'URL Protocol') { Ok 'pitcrew: is a URL protocol for this user' } else { Fail "$classes has no 'URL Protocol' value" }
  $command = (Get-ItemProperty "$classes\shell\open\command").'(default)'
  if ($command -eq $wantCommand) { Ok "pitcrew:// links open $command" } else { Fail "pitcrew:// links open '$command', not '$wantCommand'" }
} else {
  Fail "pitcrew:// is not registered ($classes is missing)"
}

# --- The AppUserModelID for notifications
$shortcut = Join-Path ([Environment]::GetFolderPath('Programs')) "$product.lnk"
if (Test-Path -LiteralPath $shortcut) {
  $shell = New-Object -ComObject Shell.Application
  $item = $shell.Namespace((Split-Path $shortcut)).ParseName((Split-Path $shortcut -Leaf))
  $aumid = $item.ExtendedProperty('System.AppUserModel.ID')
  if ($aumid -eq $identifier) { Ok "the Start menu shortcut carries the AppUserModelID $identifier" } else { Fail "the Start menu shortcut's AppUserModelID is '$aumid', not $identifier" }
} else {
  Fail "no Start menu shortcut at $shortcut"
}
if (Get-Command Get-StartApps -ErrorAction SilentlyContinue) {
  $app = @(Get-StartApps | Where-Object { $_.AppID -eq $identifier })
  if ($app.Count -gt 0) { Ok "Windows lists the app id $identifier ($($app[0].Name))" } else { Fail "Get-StartApps does not list $identifier" }
} else {
  Write-Host "::notice::Get-StartApps is not available: the app id's registration is not listed"
}

# --- The app starts with what it was installed with
$state = Join-Path $env:TEMP ('pitcrew-smoke-' + [guid]::NewGuid().ToString('N'))
$log = "$state.log"
foreach ($name in 'PITCREW_PITCREWD', 'PITCREW_ASKPASS', 'PITCREW_HELPERS', 'PITCREW_SSH') { Remove-Item "Env:$name" -ErrorAction SilentlyContinue }
$env:PITCREW_STATE_DIR = $state
$env:PITCREW_DESKTOP_LOG = 'info'
$app = Start-Process -FilePath (Join-Path $dir $appExe) -PassThru -RedirectStandardError $log -RedirectStandardOutput "$log.out"
$text = ''
for ($i = 0; $i -lt 60; $i++) {
  Start-Sleep -Seconds 1
  $text = "$(Get-Content -Raw $log -ErrorAction SilentlyContinue)"
  if (($text -like '*the main window is up*' -and $text -like '*pitcrewd is ready*') -or $app.HasExited) { break }
}
Start-Sleep -Seconds 2
$text = "$(Get-Content -Raw $log -ErrorAction SilentlyContinue)"
Stop-Process -Id $app.Id -Force -ErrorAction SilentlyContinue
Get-Process -Name pitcrewd -ErrorAction SilentlyContinue | Where-Object { $_.Path -like "$dir\*" } | Stop-Process -Force
Remove-Item Env:PITCREW_STATE_DIR, Env:PITCREW_DESKTOP_LOG
Write-Host '--- the app''s log'
Write-Host $text
Write-Host '---'
$daemonLine = 'local daemon pitcrewd=' + (Join-Path $dir 'pitcrewd.exe') + ' '
if ($text.Contains($daemonLine)) { Ok 'the app found pitcrewd next to itself' } else { Fail "the app did not log '$daemonLine'" }
foreach ($want in 'starting pitcrewd', 'pitcrewd is ready', 'the main window is up') {
  if ($text -like "*$want*") { Ok "the app's log says: $want" } else { Fail "the app's log does not say: $want" }
}
foreach ($refuse in 'no pitcrewd to start', 'remote machines cannot ask for passwords') {
  if ($text -like "*$refuse*") { Fail "the app's log says: $refuse" } else { Ok "the app's log does not say: $refuse" }
}
Start-Sleep -Seconds 2
Remove-Item -Recurse -Force $state, $log, "$log.out" -ErrorAction SilentlyContinue

# --- Install again while pitcrew-ptyd runs (an upgrade with terminals open)
$pipe = '\\.\pipe\pitcrew-check-' + [guid]::NewGuid().ToString('N')
$ptyd = Start-Process -FilePath (Join-Path $dir 'pitcrew-ptyd.exe') -PassThru -WindowStyle Hidden `
  -ArgumentList @('serve', '--endpoint', $pipe, '--foreground', '--idle-exit-ms', '600000')
Start-Sleep -Seconds 3
if ($ptyd.HasExited) {
  Fail "pitcrew-ptyd did not keep running (exit $($ptyd.ExitCode))"
} else {
  $code = Install-PitCrew
  $aside = @(Get-ChildItem -LiteralPath (Join-Path $dir '.old') -File -ErrorAction SilentlyContinue)
  if ($code -ne 0) {
    Fail "installing over a running pitcrew-ptyd exited with $code"
  } elseif ($ptyd.HasExited) {
    Fail 'installing again stopped pitcrew-ptyd'
  } elseif ($aside.Count -lt 1 -or -not (Test-Path -LiteralPath (Join-Path $dir 'pitcrew-ptyd.exe'))) {
    Fail 'installing again did not move the running pitcrew-ptyd aside and put the new one in place'
  } else {
    Ok "installed again while pitcrew-ptyd ran: it kept running from $($aside[0].FullName)"
  }
  Stop-Process -Id $ptyd.Id -Force -ErrorAction SilentlyContinue
  Start-Sleep -Seconds 2
}

# --- Remove
$p = Start-Process -FilePath (Join-Path $dir 'uninstall.exe') -ArgumentList @('/S', "_?=$dir") -Wait -PassThru
if ($p.ExitCode -ne 0) { Fail "the uninstaller exited with $($p.ExitCode)" }
if (Test-Path $classes) { Fail "pitcrew:// is still registered after removal" } else { Ok 'removal unregistered pitcrew://' }
foreach ($name in @($appExe) + $sidecars) {
  if (Test-Path -LiteralPath (Join-Path $dir $name)) { Fail "$name is still there after removal" }
}
if (Test-Path -LiteralPath (Join-Path $dir '.old')) { Fail 'the moved-aside pitcrew-ptyd was not deleted once it ended' } else { Ok 'removal deleted the programs, and the moved-aside one once it had ended' }

if ($script:failures -gt 0) {
  Write-Host "NSIS: $($script:failures) check(s) failed"
  exit 1
}
Write-Host 'NSIS: every check passed'

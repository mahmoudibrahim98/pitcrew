PitCrew @VERSION@, portable, for Windows 10 and 11 (x64)
Built from commit @COMMIT@.

PitCrew runs from this folder: no installer, no administrator, no registry keys.

START
  1. Before you unzip, unblock the zip: right-click it, Properties, tick "Unblock", OK.
     Or in PowerShell: Unblock-File .\pitcrew-windows-x64-portable.zip
     Windows marks downloaded files, and PitCrew refuses to run its own programs while
     they carry that mark.
  2. Unzip into a folder you may run programs from. Keep the files together:
     pitcrew-desktop.exe runs pitcrewd.exe, pitcrew-ptyd.exe, pitcrew-askpass.exe and
     pitcrew.exe from this same folder.
  3. Start pitcrew-desktop.exe.

  To see what PitCrew finds here without opening its window, in PowerShell:
    .\pitcrew-desktop.exe --check-layout | Out-String

CHECK THE FILES
  SHA256SUMS lists each file's SHA-256. In PowerShell, in this folder:
    Get-Content SHA256SUMS | ForEach-Object {
      $sum, $file = $_ -split '  ', 2
      if ((Get-FileHash -Algorithm SHA256 -LiteralPath $file).Hash -eq $sum) { "ok   $file" } else { "BAD  $file" }
    }
  The zip's own SHA-256 is in the summary of the GitHub Actions run that built it.

YOUR DATA
  It stays where an installed PitCrew keeps it, not in this folder:
    %LOCALAPPDATA%\PitCrew\data         the workspace (pitcrewd's state)
    %APPDATA%\org.pitcrew.desktop       settings.json and preferences.json
    %LOCALAPPDATA%\org.pitcrew.desktop  the workspace list and the window's data
  So moving to a newer zip, or to the installer later, keeps it. Only one PitCrew runs at a
  time: starting a second one brings the first one forward.

WHAT IS DIFFERENT FROM THE INSTALLED APP
  - Updates are never installed. When a new version is out, PitCrew says so and opens the
    page with the newest portable zip: unzip that into a new folder.
  - pitcrew:// links do not open PitCrew: registering them is the installer's job.
  - Notifications show as coming from Windows PowerShell: Windows shows a program's own
    name only for installed apps.
  - No helpers for remote machines: adding a machine where PitCrew is not set up yet needs
    the installed app.
  - The programs are not signed, so SmartScreen or your organisation's policy may ask
    before they run.
  - portable.txt marks this copy as portable. Keep it next to pitcrew-desktop.exe.

WEBVIEW2
  PitCrew's window uses Microsoft Edge WebView2, which Windows 10 and 11 include. If it is
  missing, PitCrew says so. Install it with Microsoft's Evergreen Bootstrapper,
  https://go.microsoft.com/fwlink/p/?LinkId=2124703, or ask your IT department.

REMOVE
  Quit PitCrew (its tray menu) and delete this folder. Delete the three folders above too to
  remove your data.

LICENCES
  LICENSE and NOTICE: PitCrew's (Apache License 2.0).
  THIRD-PARTY-NOTICES.txt: the open-source packages PitCrew is built from, and their licences.

PitCrew @VERSION@, portable, for Windows 10 and 11 (x64)
Built from commit @COMMIT@.

PitCrew runs from this folder: no installer, no administrator, no registry keys.

START
  1. Before you unzip, unblock the zip: right-click it, Properties, tick "Unblock", OK.
     Or in PowerShell: Unblock-File .\pitcrew-windows-x64-portable.zip
     Windows marks downloaded files, and PitCrew refuses to run its own programs while
     they carry that mark. Its message then says to use the installer or unblock the file:
     for this zip, unblock.
     Unzipped already? In PowerShell, in the unzipped folder:
       Get-ChildItem -Recurse | Unblock-File
  2. Unzip into a folder you may run programs from. Keep the files together:
     pitcrew-desktop.exe runs pitcrewd.exe, pitcrew-ptyd.exe, pitcrew-askpass.exe and
     pitcrew.exe from this same folder, and deploys helpers\ to remote machines.
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

UPDATES
  PitCrew never installs an update here. portable.txt says which zip this is:
  - channel=release (from a release): when a newer release is out, PitCrew says so and
    opens its page, which carries the new pitcrew-windows-x64-portable.zip.
  - channel=main (a development build): no updates are offered. Newer builds are on
    GitHub, Actions, "Portable Windows zip", successful pushes to main. Downloading
    needs a GitHub sign-in, and each build is kept for 30 days.
  To update, quit PitCrew (its tray menu), unblock the new zip, and unzip it over this
  folder, replacing the files. If Windows says a file is in use, wait until PitCrew's
  terminals have ended (pitcrew-ptyd.exe exits about 30 seconds after the last one) and
  try again. Stay in this folder: the agent hooks PitCrew installed run pitcrew.exe from
  here. If you move to another folder, install them again there, in PowerShell:
    .\pitcrew.exe hooks install

YOUR DATA
  It stays where an installed PitCrew keeps it, not in this folder:
    %LOCALAPPDATA%\PitCrew\data         the workspace (pitcrewd's state)
    %APPDATA%\org.pitcrew.desktop       settings.json and preferences.json
    %LOCALAPPDATA%\org.pitcrew.desktop  the workspace list and the window's data
  So a newer zip, or the installer later, keeps it. Only one PitCrew runs at a time:
  starting a second one brings the first one forward.

WHAT IS DIFFERENT FROM THE INSTALLED APP
  - Updates are shown, never installed (see UPDATES).
  - pitcrew:// links do not open PitCrew: registering them is the installer's job.
  - Notifications show as coming from Windows PowerShell: Windows shows a program's own
    name only for installed apps. Clicking one after PitCrew has quit opens PowerShell,
    not PitCrew: start PitCrew instead.
  - Remote machines: helpers\ holds PitCrew's helper for Linux on x86_64 and on aarch64,
    which it deploys over SSH. There is none for macOS here: adding a Mac that does not
    run PitCrew yet needs the installed app.
  - The programs are not signed, so SmartScreen or your organisation's policy may ask
    before they run.
  - portable.txt marks this copy as portable. Keep it next to pitcrew-desktop.exe.

WEBVIEW2
  PitCrew's window uses Microsoft Edge WebView2, which Windows 10 and 11 include. If it is
  missing, PitCrew says so. Install it with Microsoft's Evergreen Bootstrapper,
  https://go.microsoft.com/fwlink/p/?LinkId=2124703, or ask your IT department.

REMOVE
  First remove the agent hooks PitCrew installed, in PowerShell, in this folder:
    .\pitcrew.exe hooks uninstall
  Then quit PitCrew (its tray menu) and delete this folder. Delete the three folders under
  YOUR DATA too, to remove your data.

LICENCES
  LICENSE and NOTICE: PitCrew's (Apache License 2.0).
  THIRD-PARTY-NOTICES.txt: the open-source packages PitCrew is built from, and their licences.

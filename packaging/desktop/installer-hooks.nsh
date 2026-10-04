; Hooks for PitCrew's NSIS installer (Tauri's bundle.windows.nsis.installerHooks, set by
; packaging/desktop/build.sh). See packaging/README.md, "The desktop installers".
;
; pitcrewd.exe and pitcrew-ptyd.exe may be running while PitCrew is upgraded or removed:
; pitcrew-ptyd outlives the daemon and the app on purpose (its terminals keep running), and a
; daemon the app did not start keeps running after the app quits. Windows cannot overwrite or
; delete a running program, but it can rename one. So a program in use is moved into
; "$INSTDIR\.old" under a fresh name, where it keeps running until it ends; the next install or
; removal deletes what has ended there. Nothing is stopped.

!macro PITCREW_SWEEP_OLD
  ${If} ${FileExists} "$INSTDIR\.old\*.*"
    Delete "$INSTDIR\.old\*.*"
    RMDir "$INSTDIR\.old"
  ${EndIf}
!macroend

!macro PITCREW_MOVE_ASIDE_IF_RUNNING NAME
  ${If} ${FileExists} "$INSTDIR\${NAME}"
    ClearErrors
    ; A running program cannot be opened for writing.
    FileOpen $R9 "$INSTDIR\${NAME}" a
    ${If} ${Errors}
      CreateDirectory "$INSTDIR\.old"
      GetTempFileName $R8 "$INSTDIR\.old"
      Delete "$R8"
      ClearErrors
      Rename "$INSTDIR\${NAME}" "$R8"
      ${If} ${Errors}
        DetailPrint "${NAME} is in use and could not be moved aside"
      ${Else}
        DetailPrint "${NAME} is in use: moved aside to $R8"
      ${EndIf}
    ${Else}
      FileClose $R9
    ${EndIf}
  ${EndIf}
!macroend

!macro PITCREW_MOVE_ASIDE_ALL
  !insertmacro PITCREW_SWEEP_OLD
  !insertmacro PITCREW_MOVE_ASIDE_IF_RUNNING "pitcrewd.exe"
  !insertmacro PITCREW_MOVE_ASIDE_IF_RUNNING "pitcrew-ptyd.exe"
  !insertmacro PITCREW_MOVE_ASIDE_IF_RUNNING "pitcrew-askpass.exe"
!macroend

!macro NSIS_HOOK_PREINSTALL
  !insertmacro PITCREW_MOVE_ASIDE_ALL
  ; Previous installers stored these resources without compression. They are never run here.
  Delete "$INSTDIR\helpers\pitcrewd-x86_64-unknown-linux-musl"
  Delete "$INSTDIR\helpers\pitcrewd-aarch64-unknown-linux-musl"
  Delete "$INSTDIR\helpers\pitcrewd-universal-apple-darwin"
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  !insertmacro PITCREW_MOVE_ASIDE_ALL
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  !insertmacro PITCREW_SWEEP_OLD
  RMDir "$INSTDIR"
!macroend

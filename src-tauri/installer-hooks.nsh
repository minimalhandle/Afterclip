; Older versions installed per user, in AppData. That copy goes first, so there's only ever one.
; Settings and clips aren't part of it, so they stay.
!macro NSIS_HOOK_PREINSTALL
  ReadRegStr $R0 HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\${PRODUCTNAME}" "UninstallString"
  ${If} $R0 != ""
    nsExec::Exec `"$SYSDIR\taskkill.exe" /IM ${MAINBINARYNAME}.exe /T /F`
    Pop $R1
    ExecWait `$R0 /S`
    ; The uninstaller copies itself elsewhere and returns straight away, so give it time to finish.
    Sleep 3000
  ${EndIf}
!macroend

; The startup entry follows the app to where it's now installed.
!macro NSIS_HOOK_POSTINSTALL
  ReadRegStr $R0 HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "${PRODUCTNAME}"
  ${If} $R0 != ""
    WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "${PRODUCTNAME}" `"$INSTDIR\${MAINBINARYNAME}.exe"`
  ${EndIf}
!macroend

; An update keeps the task and startup entry. A real uninstall removes both.
!macro NSIS_HOOK_PREUNINSTALL
  ${If} $UpdateMode <> 1
    nsExec::Exec `"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -NonInteractive -Command "Get-ScheduledTask -TaskName 'Afterclip-Elevated-*' -ErrorAction SilentlyContinue | Unregister-ScheduledTask -Confirm:$$false"`
    Pop $R1
    DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "${PRODUCTNAME}"
  ${EndIf}
!macroend

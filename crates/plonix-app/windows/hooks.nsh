; Plonix installer hooks (tauri.conf.json bundle.windows.nsis.installerHooks).
;
; Puts the `plonix` command on the user's PATH: a copy of the bundled
; plonix-cli.exe as bin\plonix.exe, and that folder added to the user Path
; by user-path.ps1. Every update runs the installer again, so the copy always
; matches the app. Uninstalling takes both away.

; This file's folder, read here: inside a macro ${__FILEDIR__} would name
; the installer script the macro is expanded in.
!define PLONIX_HOOKS_DIR "${__FILEDIR__}"

!macro NSIS_HOOK_POSTINSTALL
  CreateDirectory "$INSTDIR\bin"
  Delete "$INSTDIR\bin\plonix.exe"
  CopyFiles /SILENT "$INSTDIR\plonix-cli.exe" "$INSTDIR\bin\plonix.exe"
  InitPluginsDir
  File "/oname=$PLUGINSDIR\user-path.ps1" "${PLONIX_HOOKS_DIR}\user-path.ps1"
  nsExec::Exec 'powershell -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "$PLUGINSDIR\user-path.ps1" -Folder "$INSTDIR\bin"'
  Pop $0
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  InitPluginsDir
  File "/oname=$PLUGINSDIR\user-path.ps1" "${PLONIX_HOOKS_DIR}\user-path.ps1"
  nsExec::Exec 'powershell -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "$PLUGINSDIR\user-path.ps1" -Folder "$INSTDIR\bin" -Remove'
  Pop $0
  Delete "$INSTDIR\bin\plonix.exe"
  RMDir "$INSTDIR\bin"
!macroend

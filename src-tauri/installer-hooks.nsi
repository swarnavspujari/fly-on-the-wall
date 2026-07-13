; Tauri NSIS installer hooks (bundle.windows.nsis.installerHooks).
;
; The MCP sidecar (flyonthewall-mcp.exe) is spawned directly by Claude Desktop
; and keeps running even when our app is closed. A running exe is write-locked
; on Windows, so both install (auto-update or reinstall) and uninstall fail
; with "Error opening file for writing" unless it is stopped first. The stock
; Tauri template only handles the main app process.

!macro NSIS_HOOK_PREINSTALL
  nsExec::Exec 'taskkill /F /IM flyonthewall-mcp.exe /T'
  Pop $0
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  nsExec::Exec 'taskkill /F /IM flyonthewall-mcp.exe /T'
  Pop $0
!macroend

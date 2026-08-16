; DeepSeek Harness desktop — NSIS installer hooks.
;
; The app hides to the system tray when its window is closed instead of
; exiting, and its harness server keeps running from the install directory
; (tools\node\node.exe). A plain upgrade then fails with
; "无法打开要写入的文件 ... node.exe" because the running process locks the
; file. Kill the old shell (and its server process tree) before the installer
; copies files or the uninstaller removes them.

!macro NSIS_HOOK_PREINSTALL
  ExecWait 'cmd /c taskkill /F /T /IM dsh-desktop.exe'
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  ExecWait 'cmd /c taskkill /F /T /IM dsh-desktop.exe'
!macroend

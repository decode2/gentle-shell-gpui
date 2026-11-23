@echo off
rem gentle-shell-gpui gsd shim (installed by the Windows installer)
rem `gsd` opens the current folder as a workspace, `gsd <folder>` that folder.
start "" "%~dp0..\gentle-shell-gpui.exe" --cwd %*

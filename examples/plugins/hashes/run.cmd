@echo off
rem A plugin's `run` must be a file beside plugin.json, and CreateProcess cannot
rem start a .ps1 directly. So this is the launcher, and it is also where a
rem scripted plugin declares its interpreter. Standard input passes straight
rem through to the script.
powershell -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "%~dp0hashes.ps1"

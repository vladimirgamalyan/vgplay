# Removes vgplay associations and deletes the installed folder.
param([string]$InstallDir = "$env:LOCALAPPDATA\Programs\vgplay")
$ErrorActionPreference = "Stop"

Get-Process vgplay -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 200

if (Test-Path "$InstallDir\vgplay.exe") {
    Start-Process -FilePath "$InstallDir\vgplay.exe" -ArgumentList "--unregister" -Wait -NoNewWindow
    Remove-Item -Recurse -Force $InstallDir
    Write-Host "vgplay removed from $InstallDir, associations cleared." -ForegroundColor Green
    Write-Host "If sound files were set to open with vgplay by default, Windows will prompt for an app again."
} else {
    Write-Host "Not found: $InstallDir (nothing to remove)."
}

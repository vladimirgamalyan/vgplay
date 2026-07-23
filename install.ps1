# Intermediate vgplay release: build → install into a per-user folder → register associations.
# By default installs into %LOCALAPPDATA%\Programs\vgplay (no admin rights required).
# To install into Program Files, run from an elevated terminal:
#   powershell -File install.ps1 -InstallDir "C:\Program Files\vgplay"
param([string]$InstallDir = "$env:LOCALAPPDATA\Programs\vgplay")
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $MyInvocation.MyCommand.Path
$cargo = "$env:USERPROFILE\.cargo\bin\cargo.exe"

Write-Host "1/4  Stopping running vgplay (so the .exe is not locked)..."
Get-Process vgplay -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 200

Write-Host "2/4  Building release..."
& $cargo build --release --manifest-path "$root\Cargo.toml"
if ($LASTEXITCODE -ne 0) { throw "cargo build failed (code $LASTEXITCODE)" }

Write-Host "3/4  Copying to $InstallDir ..."
New-Item -ItemType Directory -Force $InstallDir | Out-Null
Copy-Item "$root\target\release\vgplay.exe" "$InstallDir\vgplay.exe" -Force

Write-Host "4/4  Registering associations (HKCU)..."
Start-Process -FilePath "$InstallDir\vgplay.exe" -ArgumentList "--register" -Wait -NoNewWindow

Write-Host ""
Write-Host "Done: $InstallDir\vgplay.exe" -ForegroundColor Green
Write-Host "First time, set it as default (Windows 11 requires manual confirmation):" -ForegroundColor Yellow
Write-Host "  Right-click a sound file -> Open with -> Choose another app -> vgplay -> Always."
Write-Host "After that, for each new release just run install.ps1 again; the .exe path does not change."

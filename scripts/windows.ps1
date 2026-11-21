<#
.SYNOPSIS
    Build, bundle, install and uninstall Rewind on Windows: the counterpart
    of `make bundle`, `make install` and `make uninstall`.

.EXAMPLE
    .\scripts\windows.ps1 bundle      # dist\Rewind\ with all four executables
    .\scripts\windows.ps1 install     # %LOCALAPPDATA%\Programs\Rewind, Start at Login, started
    .\scripts\windows.ps1 uninstall
#>
param(
    [Parameter(Position = 0)]
    [ValidateSet("bundle", "install", "uninstall")]
    [string]$Action = "bundle"
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$dist = Join-Path $root "dist\Rewind"
$target = Join-Path $env:LOCALAPPDATA "Programs\Rewind"

function Stop-Rewind {
    # Ask politely first so the tray icon is removed, then make sure.
    $ctl = Join-Path $target "rewindctl.exe"
    if (Test-Path $ctl) { & $ctl quit 2>$null | Out-Null; Start-Sleep -Milliseconds 800 }
    Get-Process Rewind, rewindd, rewind-ui -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
}

function Invoke-Cargo([string]$crate) {
    # Windows PowerShell turns cargo's progress on stderr into error records,
    # so judge by the exit code, not by $? or the error stream.
    Push-Location (Join-Path $root $crate)
    $previous = $ErrorActionPreference; $ErrorActionPreference = "Continue"
    cargo build --release 2>&1 | ForEach-Object { "$_" } | Where-Object { $_ -match "^(error|warning: unused)" }
    $code = $LASTEXITCODE
    $ErrorActionPreference = $previous; Pop-Location
    if ($code -ne 0) { throw "cargo build --release failed in $crate" }
}

function Build-Bundle {
    Invoke-Cargo "win"
    Invoke-Cargo "ui"

    New-Item -ItemType Directory -Force $dist | Out-Null
    foreach ($exe in "Rewind.exe", "rewindd.exe", "rewindctl.exe") {
        Copy-Item (Join-Path $root "win\target\release\$exe") $dist -Force
    }
    Copy-Item (Join-Path $root "ui\target\release\rewind-ui.exe") $dist -Force
    Write-Host "built $dist"
}

switch ($Action) {
    "bundle" { Build-Bundle }
    "install" {
        Build-Bundle
        Stop-Rewind
        New-Item -ItemType Directory -Force $target | Out-Null
        Copy-Item (Join-Path $dist "*") $target -Force
        Write-Host "installed $target"
        & (Join-Path $target "rewindd.exe") login-item on
        Start-Process (Join-Path $target "Rewind.exe")
        Write-Host "Rewind is running. Alt+Shift+S opens it; the tray icon may start in the overflow (^)."
    }
    "uninstall" {
        if (Test-Path (Join-Path $target "rewindd.exe")) { & (Join-Path $target "rewindd.exe") login-item off }
        Stop-Rewind
        Remove-Item -Recurse -Force $target -ErrorAction SilentlyContinue
        Write-Host "removed $target"
    }
}

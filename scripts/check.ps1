#!/usr/bin/env pwsh
# Windows entry point for the check runner. Mirrors check.sh: same flags, same
# exit code, run from the runner's own directory so `go run .` finds the module.
$ErrorActionPreference = 'Stop'

$checkDir = Join-Path $PSScriptRoot 'check'

Push-Location $checkDir
try {
    & go run . @args
    exit $LASTEXITCODE
} finally {
    Pop-Location
}

# Authenticode-sign one file. Tauri runs this through bundle.windows.signCommand
# for the app binary and each installer.
#
# Set one identity in the environment:
#   Certificate (OV/EV, on a token or in the Windows store):
#     SPLITVIEW_SIGN_THUMBPRINT   the certificate's SHA-1 thumbprint
#   Azure Artifact Signing:
#     SPLITVIEW_SIGN_DLIB         path to Azure.CodeSigning.Dlib.dll
#     SPLITVIEW_SIGN_METADATA     path to the metadata.json for your account
#
# With no identity the file is left unsigned, so everyday builds still work. Set
# SPLITVIEW_SIGN_REQUIRED=1 for a release build to fail instead.

param(
    [Parameter(Mandatory = $true)]
    [string]$Path
)

$ErrorActionPreference = "Stop"

function Find-SignTool {
    $onPath = Get-Command signtool.exe -ErrorAction SilentlyContinue
    if ($onPath) { return $onPath.Source }

    $kits = Join-Path ${env:ProgramFiles(x86)} "Windows Kits\10\bin"
    $found = Get-ChildItem $kits -Filter signtool.exe -Recurse -ErrorAction SilentlyContinue |
        Where-Object { $_.Directory.Name -eq "x64" } |
        Sort-Object { $_.Directory.Parent.Name -as [version] } |
        Select-Object -Last 1
    if ($found) { return $found.FullName }

    throw "signtool.exe not found. Install the Windows SDK."
}

$thumbprint = $env:SPLITVIEW_SIGN_THUMBPRINT
$dlib = $env:SPLITVIEW_SIGN_DLIB
$metadata = $env:SPLITVIEW_SIGN_METADATA

if ($thumbprint) {
    $identity = @("/sha1", $thumbprint)
    $timestamp = "http://timestamp.digicert.com"
} elseif ($dlib -and $metadata) {
    $identity = @("/dlib", $dlib, "/dmdf", $metadata)
    $timestamp = "http://timestamp.acs.microsoft.com"
} elseif ($env:SPLITVIEW_SIGN_REQUIRED -eq "1") {
    Write-Error "SPLITVIEW_SIGN_REQUIRED is set but no signing identity is configured."
    exit 1
} else {
    Write-Warning "No signing identity configured; leaving $Path unsigned."
    exit 0
}

$signtool = Find-SignTool
& $signtool sign /v /fd SHA256 /tr $timestamp /td SHA256 @identity $Path
exit $LASTEXITCODE

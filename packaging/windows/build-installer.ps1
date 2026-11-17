# Builds dist\SplitView_<version>_x64-setup.exe:
#   release build -> sign splitview.exe -> Inno Setup -> sign the installer.
# Signing is skipped (with a warning) unless an identity is configured; see
# sign-windows.ps1. Run from anywhere:  powershell -File packaging\windows\build-installer.ps1
param([switch]$SkipBuild)
$ErrorActionPreference = 'Stop'
$root = Resolve-Path "$PSScriptRoot\..\.."
$version = (Select-String -Path "$root\Cargo.toml" -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value

if (-not $SkipBuild) {
    Push-Location $root
    try {
        cargo build --release -p splitview
        if ($LASTEXITCODE) { throw "cargo build failed ($LASTEXITCODE)" }
    } finally { Pop-Location }
}
$exe = "$root\target\release\splitview.exe"
if (-not (Test-Path $exe)) { throw "missing $exe" }

& "$PSScriptRoot\sign-windows.ps1" -Path $exe
if ($LASTEXITCODE) { throw "signing splitview.exe failed" }

$iscc = @(
    (Get-Command ISCC.exe -ErrorAction SilentlyContinue).Source,
    "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe",
    "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
    "$env:ProgramFiles\Inno Setup 6\ISCC.exe"
) | Where-Object { $_ -and (Test-Path $_) } | Select-Object -First 1
if (-not $iscc) { throw 'Inno Setup 6 (ISCC.exe) not found' }

& $iscc /Q "/DAppVersion=$version" "$PSScriptRoot\splitview.iss"
if ($LASTEXITCODE) { throw "ISCC failed ($LASTEXITCODE)" }

$setup = "$root\dist\SplitView_${version}_x64-setup.exe"
& "$PSScriptRoot\sign-windows.ps1" -Path $setup
if ($LASTEXITCODE) { throw "signing the installer failed" }

$item = Get-Item $setup
$hash = (Get-FileHash $setup -Algorithm SHA256).Hash.ToLower()
"{0}  {1:N1} MB  sha256 {2}" -f $item.FullName, ($item.Length / 1MB), $hash

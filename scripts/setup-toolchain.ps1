$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$tools = 'D:\tools'
$buildToolsPath = "$tools\vs-buildtools"
$rustupHome = "$tools\rust\rustup"
$cargoHome = "$tools\rust\cargo"
$rustupUrl = 'https://static.rust-lang.org/rustup/dist/x86_64-pc-windows-msvc/rustup-init.exe'
$vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"

function Set-UserVariable([string]$name, [string]$value) {
    [Environment]::SetEnvironmentVariable($name, $value, 'User')
    Set-Item "env:$name" $value
}

function Assert-Sha256([string]$file, [string]$expected) {
    $actual = (Get-FileHash $file -Algorithm SHA256).Hash.ToLower()
    if ($actual -ne $expected.ToLower()) { throw "Checksum mismatch for $file`n expected $expected`n actual   $actual" }
}

function Test-MsvcInstalled {
    if (-not (Test-Path $vswhere)) { return $false }
    $found = & $vswhere -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
    return [bool]$found
}

if (-not (Test-MsvcInstalled)) {
    Write-Output "Installing VS Build Tools (C++ x64 + Windows SDK) to $buildToolsPath (UAC prompt will appear) ..."
    winget install --id Microsoft.VisualStudio.2022.BuildTools --exact --accept-package-agreements --accept-source-agreements `
        --override "--passive --wait --norestart --nocache --installPath $buildToolsPath --add Microsoft.VisualStudio.Component.VC.Tools.x86.x64 --add Microsoft.VisualStudio.Component.Windows11SDK.22621"
    if (-not (Test-MsvcInstalled)) { throw 'VS Build Tools installation did not complete' }
}

Set-UserVariable 'RUSTUP_HOME' $rustupHome
Set-UserVariable 'CARGO_HOME' $cargoHome

if (-not (Test-Path "$cargoHome\bin\cargo.exe")) {
    $installer = "$tools\rust\installer\rustup-init.exe"
    New-Item -ItemType Directory -Force (Split-Path $installer) | Out-Null
    if (-not (Test-Path $installer)) { Invoke-WebRequest $rustupUrl -OutFile $installer -UseBasicParsing }
    $checksum = (Invoke-WebRequest "$rustupUrl.sha256" -UseBasicParsing).Content
    if ($checksum -is [byte[]]) { $checksum = [Text.Encoding]::ASCII.GetString($checksum) }
    Assert-Sha256 $installer ($checksum -split '\s+')[0]
    Write-Output "Installing Rust (MSVC toolchain) to $tools\rust ..."
    & $installer -y --profile minimal --default-host x86_64-pc-windows-msvc --default-toolchain stable
    if ($LASTEXITCODE -ne 0) { throw "rustup-init failed with exit code $LASTEXITCODE" }
}

Write-Output ''
& "$cargoHome\bin\rustc.exe" -vV | Select-String 'host|release'
Write-Output 'Done. Open a NEW terminal so PATH changes apply.'

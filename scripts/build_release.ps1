# Builds the 0.1.0 Windows x64 release into dist/ (portable exe files only).
# Requirements: Rust (MSVC), VS Build Tools (C++), WebView2, cargo-tauri in PATH.
# Run: powershell -ExecutionPolicy Bypass -File scripts\build_release.ps1

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

# rustup installs cargo into %USERPROFILE%\.cargo\bin; already running shells keep an old PATH.
$env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"

$version = '0.1.0'
$name = "Shtorka-$version-win64"
$dist = Join-Path $root 'dist'
$portable = Join-Path $dist $name
$target = Join-Path $root 'target\release'

function Find-VcVars {
    $candidates = Get-ChildItem "${env:ProgramFiles(x86)}\Microsoft Visual Studio\2022" -Directory -ErrorAction SilentlyContinue |
        ForEach-Object { Join-Path $_.FullName 'VC\Auxiliary\Build\vcvars64.bat' } |
        Where-Object { Test-Path $_ }
    return $candidates | Select-Object -First 1
}

$vcvars = Find-VcVars
if ($vcvars) {
    Write-Host "MSVC: $vcvars"
    cmd /c "call `"$vcvars`" >nul 2>&1 && set" | ForEach-Object {
        if ($_ -match '^([^=]+)=(.*)$') { Set-Item -Path "env:$($matches[1])" -Value $matches[2] }
    }
} else {
    Write-Warning 'vcvars64.bat not found - cargo may not find link.exe'
}

Write-Host '==> cargo test (workspace)'
cargo test --workspace
if ($LASTEXITCODE -ne 0) { throw "cargo test failed with exit code $LASTEXITCODE" }

Write-Host '==> build server.exe'
cargo build --release -p server
if ($LASTEXITCODE -ne 0) { throw "server build failed with exit code $LASTEXITCODE" }

Write-Host '==> build Shtorka client (no installers)'
Push-Location (Join-Path $root 'client')
try {
    cargo tauri build --no-bundle
    if ($LASTEXITCODE -ne 0) { throw "client build failed with exit code $LASTEXITCODE" }
} finally { Pop-Location }

Write-Host '==> build Shtorka Server launcher (no installers)'
Push-Location (Join-Path $root 'launcher')
try {
    cargo tauri build --no-bundle
    if ($LASTEXITCODE -ne 0) { throw "launcher build failed with exit code $LASTEXITCODE" }
} finally { Pop-Location }

Write-Host '==> check runtime dependencies'
$exes = @(
    (Join-Path $target 'server.exe'),
    (Join-Path $target 'messenger.exe'),
    (Join-Path $target 'messenger-server.exe')
)
foreach ($exe in $exes) {
    if (-not (Test-Path $exe)) { throw "Missing build output: $exe" }
    $deps = & dumpbin /nologo /dependents $exe 2>&1 | Out-String
    if ($deps -match 'vcruntime|msvcp|api-ms-win-crt') {
        Write-Host $deps
        throw "Dynamic CRT dependency found in $exe"
    }
    Write-Host "    OK: $(Split-Path -Leaf $exe) has no VC++ runtime dependency"
}

Write-Host '==> package dist'
if (Test-Path $dist) { Remove-Item $dist -Recurse -Force }
New-Item -ItemType Directory -Path $portable -Force | Out-Null

Copy-Item (Join-Path $target 'server.exe') $portable
Copy-Item (Join-Path $target 'messenger.exe') (Join-Path $portable 'Shtorka.exe')
Copy-Item (Join-Path $target 'messenger-server.exe') (Join-Path $portable 'Shtorka-Server.exe')

# Standalone files for the GitHub release page.
Copy-Item (Join-Path $target 'server.exe') (Join-Path $dist 'server.exe')
Copy-Item (Join-Path $target 'messenger.exe') (Join-Path $dist 'Shtorka.exe')
Copy-Item (Join-Path $target 'messenger-server.exe') (Join-Path $dist 'Shtorka-Server.exe')

$readme = Get-ChildItem (Join-Path $root 'scripts') -Filter '*.txt' | Select-Object -First 1
if ($readme) {
    Copy-Item $readme.FullName (Join-Path $portable $readme.Name) -Force
    Copy-Item $readme.FullName (Join-Path $dist $readme.Name) -Force
}
Copy-Item (Join-Path $root "RELEASE-$version.md") (Join-Path $dist "RELEASE-$version.md") -Force
Copy-Item (Join-Path $root "RELEASE-$version.md") (Join-Path $portable "RELEASE-$version.md") -Force

# Zip with forward slashes (Compress-Archive writes backslashes and breaks names on Linux/macOS).
Add-Type -AssemblyName System.IO.Compression
Add-Type -AssemblyName System.IO.Compression.FileSystem
$zipPath = Join-Path $dist "$name.zip"
$fs = [IO.File]::Open($zipPath, [IO.FileMode]::CreateNew)
$zip = New-Object IO.Compression.ZipArchive($fs, [IO.Compression.ZipArchiveMode]::Create, $false, [Text.Encoding]::UTF8)
foreach ($f in Get-ChildItem $portable -Recurse -File) {
    $rel = $f.FullName.Substring($portable.Length + 1).Replace('\', '/')
    $entry = $zip.CreateEntry("$name/$rel", [IO.Compression.CompressionLevel]::Optimal)
    $out = $entry.Open()
    $in = [IO.File]::OpenRead($f.FullName)
    $in.CopyTo($out)
    $in.Dispose(); $out.Dispose()
}
$zip.Dispose(); $fs.Dispose()

$hashLines = Get-ChildItem $dist -File | Where-Object { $_.Name -ne 'SHA256SUMS.txt' } | Sort-Object Name | ForEach-Object {
    '{0}  {1}' -f (Get-FileHash $_.FullName -Algorithm SHA256).Hash.ToLower(), $_.Name
}
$utf8 = New-Object System.Text.UTF8Encoding $false
[IO.File]::WriteAllText((Join-Path $dist 'SHA256SUMS.txt'), ($hashLines -join "`r`n") + "`r`n", $utf8)

Write-Host ''
Write-Host "Done: $dist"
Get-ChildItem $dist -File | Sort-Object Name | Select-Object Name, @{n = 'MB'; e = { [math]::Round($_.Length / 1MB, 2) } }
exit 0

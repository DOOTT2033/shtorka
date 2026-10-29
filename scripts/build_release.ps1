# Собирает релиз 0.0.2 для Windows x64 в dist/.
# Требования: Rust (MSVC), VS Build Tools (C++), WebView2, cargo-tauri в PATH.
# Запуск: powershell -ExecutionPolicy Bypass -File scripts\build_release.ps1

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

# rustup ставит cargo в %USERPROFILE%\.cargo\bin, но у уже запущенных процессов PATH старый.
$env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"

$version = '0.0.2'
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
    Write-Warning 'vcvars64.bat не найден — cargo может не найти link.exe'
}

Write-Host '==> cargo test (server)'
cargo test --release -p server
Write-Host '==> cargo test (launcher)'
cargo test -p launcher
Write-Host '==> сборка server.exe'
cargo build --release -p server

Write-Host '==> сборка лаунчера и клиента (Tauri)'
Push-Location (Join-Path $root 'launcher'); cargo tauri build; Pop-Location
Push-Location (Join-Path $root 'client');   cargo tauri build; Pop-Location

Write-Host '==> упаковка dist'
if (Test-Path $portable) { Remove-Item $portable -Recurse -Force }
New-Item -ItemType Directory -Path (Join-Path $portable 'installers') -Force | Out-Null
Copy-Item (Join-Path $target 'server.exe') $portable
Copy-Item (Join-Path $target 'messenger.exe') (Join-Path $portable 'Shtorka.exe')
Copy-Item (Join-Path $target 'messenger-server.exe') (Join-Path $portable 'Shtorka-Server.exe')
# Отдельные exe для страницы релиза (как в 0.0.1-alpha).
Copy-Item (Join-Path $target 'messenger.exe') (Join-Path $dist 'Shtorka.exe')
Copy-Item (Join-Path $target 'messenger-server.exe') (Join-Path $dist 'Shtorka-Server.exe')

$bundles = @(
    @{ src = 'bundle\nsis\Shtorka_0.0.2_x64-setup.exe';             dst = 'Shtorka_0.0.2_x64-setup.exe' },
    @{ src = 'bundle\msi\Shtorka_0.0.2_x64_en-US.msi';              dst = 'Shtorka_0.0.2_x64_en-US.msi' },
    @{ src = 'bundle\nsis\Shtorka Server_0.0.2_x64-setup.exe';      dst = 'Shtorka-Server_0.0.2_x64-setup.exe' },
    @{ src = 'bundle\msi\Shtorka Server_0.0.2_x64_en-US.msi';       dst = 'Shtorka-Server_0.0.2_x64_en-US.msi' }
)
foreach ($b in $bundles) {
    $src = Join-Path $target $b.src
    Copy-Item $src (Join-Path $portable ('installers\' + $b.dst)) -Force
    Copy-Item $src (Join-Path $dist $b.dst) -Force
}

Copy-Item (Join-Path $root 'RELEASE-0.0.2.md') (Join-Path $dist 'RELEASE-0.0.2.md') -Force
Copy-Item (Join-Path $root 'scripts\ПРОЧТИ-МЕНЯ.txt') (Join-Path $portable 'ПРОЧТИ-МЕНЯ.txt') -Force

# Zip с прямыми слэшами (Compress-Archive пишет обратные и ломает имена на Linux/macOS).
Add-Type -AssemblyName System.IO.Compression
Add-Type -AssemblyName System.IO.Compression.FileSystem
$zipPath = Join-Path $dist "$name.zip"
Remove-Item $zipPath -Force -ErrorAction SilentlyContinue
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

Get-ChildItem $dist -File | Where-Object { $_.Name -ne 'SHA256SUMS.txt' } | Sort-Object Name | ForEach-Object {
    '{0}  {1}' -f (Get-FileHash $_.FullName -Algorithm SHA256).Hash.ToLower(), $_.Name
} | Out-File (Join-Path $dist 'SHA256SUMS.txt') -Encoding ascii

Write-Host ''
Write-Host "Готово: $dist"
Get-ChildItem $dist -File | Sort-Object Name | Select-Object Name, @{n = 'MB'; e = { [math]::Round($_.Length / 1MB, 2) } }

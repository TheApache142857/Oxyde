# Builds the Oxyde bundle for Windows: dist\Oxyde\ (Oxyde.exe, the OpenCASCADE DLLs it
# needs, licences), dist\Oxyde-<version>-windows-x64.zip and, when the tools are there,
# the installers: dist\Oxyde-Setup-<version>.exe (Inno Setup's iscc, per user) and
# dist\Oxyde-<version>-windows-x64.msi (the WiX Toolset's wix, version 5 or later, for
# every user of the machine).
#
#   powershell -ExecutionPolicy Bypass -File packaging\bundle.ps1
#
# OpenCASCADE is found as for building (see docs\BUILDING.md): OXYDE_OCCT_DIR, VCPKG_ROOT,
# or OXYDE_OCCT_LIB_DIR. Its DLLs are taken from the matching bin folder.

$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $PSScriptRoot
Set-Location $Root

# ---- Version, from the workspace's Cargo.toml -----------------------------------------------
$Version = (Select-String -Path Cargo.toml -Pattern '^version\s*=\s*"([^"]+)"' |
    Select-Object -First 1).Matches[0].Groups[1].Value
Write-Host "Oxyde $Version"

# ---- Build ----------------------------------------------------------------------------------
cargo build --release -p oxyde-app
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

# ---- Where the OpenCASCADE DLLs are ---------------------------------------------------------
function Find-OcctBin {
    $roots = @()
    if ($env:OXYDE_OCCT_LIB_DIR) { $roots += (Join-Path $env:OXYDE_OCCT_LIB_DIR "..\bin") }
    if ($env:OXYDE_OCCT_DIR) {
        $roots += (Join-Path $env:OXYDE_OCCT_DIR "bin")
        $roots += (Join-Path $env:OXYDE_OCCT_DIR "win64\vc14\bin")
    }
    if ($env:VCPKG_ROOT) { $roots += (Join-Path $env:VCPKG_ROOT "installed\x64-windows\bin") }
    foreach ($r in $roots) {
        if (Test-Path (Join-Path $r "TKernel.dll")) { return (Resolve-Path $r).Path }
    }
    throw "OpenCASCADE's bin folder (with TKernel.dll) not found: set OXYDE_OCCT_DIR or VCPKG_ROOT"
}
$OcctBin = Find-OcctBin
Write-Host "OpenCASCADE DLLs from $OcctBin"

# ---- dumpbin, to follow DLL dependencies ----------------------------------------------------
function Find-Dumpbin {
    $cmd = Get-Command dumpbin.exe -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
    $vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
    if (Test-Path $vswhere) {
        $vs = & $vswhere -latest -products * -property installationPath
        $found = Get-ChildItem -Path (Join-Path $vs "VC\Tools\MSVC") -Recurse `
            -Filter dumpbin.exe -ErrorAction SilentlyContinue |
            Where-Object { $_.FullName -match "Hostx64\\x64" } | Select-Object -First 1
        if ($found) { return $found.FullName }
    }
    return $null
}
$Dumpbin = Find-Dumpbin

# ---- Assemble dist\Oxyde --------------------------------------------------------------------
$Dist = Join-Path $Root "dist"
$Bundle = Join-Path $Dist "Oxyde"
if (Test-Path $Bundle) { Remove-Item -Recurse -Force $Bundle }
New-Item -ItemType Directory -Force -Path $Bundle | Out-Null
Copy-Item target\release\oxyde.exe (Join-Path $Bundle "Oxyde.exe")

if ($Dumpbin) {
    # Every DLL the exe needs, then theirs, that OpenCASCADE's bin folder has (system DLLs
    # stay out).
    $queue = New-Object System.Collections.Queue
    $queue.Enqueue((Join-Path $Bundle "Oxyde.exe"))
    $copied = @{}
    while ($queue.Count -gt 0) {
        $file = $queue.Dequeue()
        $deps = & $Dumpbin /nologo /dependents $file |
            Where-Object { $_ -match '^\s+(\S+\.dll)\s*$' } | ForEach-Object { $Matches[1] }
        foreach ($dll in $deps) {
            $source = Join-Path $OcctBin $dll
            if (-not $copied.ContainsKey($dll.ToLower()) -and (Test-Path $source)) {
                Copy-Item $source $Bundle
                $copied[$dll.ToLower()] = $true
                $queue.Enqueue((Join-Path $Bundle $dll))
            }
        }
    }
    Write-Host "$($copied.Count) DLLs copied"
} else {
    Write-Warning "dumpbin not found: copying every DLL of $OcctBin"
    Copy-Item (Join-Path $OcctBin "*.dll") $Bundle
}

Copy-Item LICENSE (Join-Path $Bundle "LICENSE.txt")
Copy-Item packaging\THIRD-PARTY.txt $Bundle
Copy-Item README.md $Bundle

# ---- Smoke test: the exe must start and find every DLL --------------------------------------
$probe = Start-Process -FilePath (Join-Path $Bundle "Oxyde.exe") -ArgumentList "--version" `
    -PassThru -Wait -NoNewWindow -RedirectStandardOutput (Join-Path $Dist "version.txt")
if ($probe.ExitCode -ne 0) { throw "Oxyde.exe --version failed (a missing DLL?)" }
Write-Host ("Bundle runs: " + (Get-Content (Join-Path $Dist "version.txt")))

# ---- Zip and installer ----------------------------------------------------------------------
$Zip = Join-Path $Dist "Oxyde-$Version-windows-x64.zip"
if (Test-Path $Zip) { Remove-Item $Zip }
Compress-Archive -Path $Bundle -DestinationPath $Zip
Write-Host "Wrote $Zip"

# The tools are kept as paths: Get-Command gives a command whose .Source is its path,
# Get-Item a file, which has none.
$iscc = (Get-Command iscc.exe -ErrorAction SilentlyContinue).Source
if (-not $iscc) {
    $default = "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe"
    if (Test-Path $default) { $iscc = $default }
}
if ($iscc) {
    & $iscc "/DAppVersion=$Version" packaging\installer.iss
    if ($LASTEXITCODE -ne 0) { throw "iscc failed" }
} else {
    Write-Warning "Inno Setup (iscc) not found: no .exe installer"
}

# ---- Windows Installer package (.msi) -------------------------------------------------------
# WiX is a .NET tool: `dotnet tool install --global wix --version 5.0.2` (needs the .NET
# SDK 8 or later). WiX 6 works too but asks you to accept its maintenance-fee licence
# first (`wix eula accept wix6`).
# A terminal opened before WiX was installed does not have .dotnet\tools on its PATH yet.
$wix = (Get-Command wix.exe -ErrorAction SilentlyContinue).Source
if (-not $wix) {
    $tool = Join-Path $env:USERPROFILE ".dotnet\tools\wix.exe"
    if (Test-Path $tool) { $wix = $tool }
}
if ($wix) {
    $Msi = Join-Path $Dist "Oxyde-$Version-windows-x64.msi"
    if (Test-Path $Msi) { Remove-Item $Msi }
    # WiX's working files go next to the bundle: in its default random folder under %TEMP%
    # the cabinet it was writing vanished mid-build ("failed to compress cabinet").
    $WixObj = Join-Path $Dist "wix-obj"
    & $wix build packaging\installer.wxs -arch x64 -d "Version=$Version" `
        -d "BundleDir=$Bundle" -intermediatefolder $WixObj -o $Msi
    if ($LASTEXITCODE -ne 0) { throw "wix build failed" }
    Remove-Item -Recurse -Force $WixObj -ErrorAction SilentlyContinue
    # The debug file WiX writes next to the .msi is not for users.
    Remove-Item ([IO.Path]::ChangeExtension($Msi, ".wixpdb")) -ErrorAction SilentlyContinue
    Write-Host "Wrote $Msi"
} else {
    Write-Warning "WiX (wix) not found: no .msi. Install it with: dotnet tool install --global wix --version 5.0.2"
}

<#
.SYNOPSIS
Builds the Windows release folder for Lillignode.

.DESCRIPTION
Builds lillignode.exe and puts it in dist\lillignode-<version>-windows-x64\ with
the two Microsoft pseudo-terminal files Claude Code's status needs on Windows
(conpty.dll and OpenConsole.exe), the MIT notice for those
two files, the README and the LICENCE. Then zips that folder into
dist\lillignode-<version>-windows-x64.zip and writes the zip's SHA-256 beside it.

The Microsoft files come from one pinned package on the Windows Terminal
releases page. Each file must match its recorded SHA-256 hash AND carry a
valid Microsoft signature. If either check fails for either file, nothing is
packaged and the script exits with an error.

Run from anywhere:

    powershell -ExecutionPolicy Bypass -File scripts\package-windows.ps1

See docs\packaging-windows.md.

.PARAMETER Package
A copy of the pinned package already on this machine, to use instead of
downloading it. It is checked exactly as a downloaded one is.

.PARAMETER BesideBuild
Also place the two checked Microsoft files beside target\release\lillignode.exe,
so a Lillignode run straight from the build folder shows Claude Code's status.
For development; the release folder is built either way.
#>
[CmdletBinding()]
param(
    [string]$Package,
    [switch]$BesideBuild
)

Set-StrictMode -Version 2
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

# ---------------------------------------------------------------------------
# The pin. Changing any of these values is a
# decision, not a chore: record it in docs\packaging-windows.md and repeat the
# status check against real Claude Code by hand.
# ---------------------------------------------------------------------------
$PinnedRelease = 'v1.25.3131.0'
$PackageName   = 'Microsoft.Windows.Console.ConPTY.1.25.261109001.nupkg'
$PackageUrl    = "https://github.com/microsoft/terminal/releases/download/$PinnedRelease/$PackageName"
$PackageSha256 = '2d4f846e9b59ab85977d4d15022c696b36e5400f6320608be1c75d3d0195fee7'
$RequiredSigner = 'Microsoft Corporation'
$MicrosoftFiles = @(
    @{ Name   = 'conpty.dll'
       Entry  = 'runtimes/win-x64/native/conpty.dll'
       Sha256 = 'e971856f89748dc3b458125e9afdd79b5e9574a7c937676116d3cbbd3b71f492' },
    @{ Name   = 'OpenConsole.exe'
       Entry  = 'build/native/runtimes/x64/OpenConsole.exe'
       Sha256 = 'b17e48b07ef153b420f0a6e178bacd31fb4c3390e21ae746586213fc0719a775' }
)
$NoticeName = 'LICENSE-Microsoft-ConPTY.txt'

function Get-Sha256([string]$Path) {
    (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

# Throws unless the file has exactly the recorded hash.
function Assert-Hash([string]$Path, [string]$Expected, [string]$What) {
    $actual = Get-Sha256 $Path
    if ($actual -ne $Expected.ToLowerInvariant()) {
        throw "$What does not match its recorded SHA-256 hash.`n  expected $Expected`n  found    $actual`n  file     $Path"
    }
}

# Throws unless Windows says the file's signature is valid and the signer is
# Microsoft Corporation. Independent of the hash: it does not read the pin.
function Assert-MicrosoftSignature([string]$Path, [string]$What) {
    $signature = Get-AuthenticodeSignature -LiteralPath $Path
    if ($signature.Status -ne 'Valid') {
        throw "$What does not carry a valid signature (Windows reports: $($signature.Status)).`n  file     $Path"
    }
    $signer = $signature.SignerCertificate.GetNameInfo('SimpleName', $false)
    if ($signer -ne $RequiredSigner) {
        throw "$What is signed by `"$signer`", not by $RequiredSigner.`n  file     $Path"
    }
}

# Runs a program with its error output folded into its output, so Windows
# PowerShell does not mistake progress lines for failures. Returns the lines.
function Invoke-Program([string]$CommandLine) {
    $lines = & cmd.exe /d /c "$CommandLine 2>&1"
    if ($LASTEXITCODE -ne 0) {
        throw "``$CommandLine`` failed with exit code $LASTEXITCODE.`n$($lines -join "`n")"
    }
    $lines
}

# The pinned package, checked against its hash. Downloaded once and kept in
# target\package-cache; a kept copy that no longer matches is fetched again.
function Get-PinnedPackage([string]$CacheDir, [string]$Given) {
    if ($Given) {
        if (-not (Test-Path -LiteralPath $Given -PathType Leaf)) {
            throw "The package given with -Package is not a file: $Given"
        }
        Assert-Hash $Given $PackageSha256 "The package given with -Package"
        return (Resolve-Path -LiteralPath $Given).Path
    }
    $cached = Join-Path $CacheDir $PackageName
    if (Test-Path -LiteralPath $cached -PathType Leaf) {
        if ((Get-Sha256 $cached) -eq $PackageSha256) {
            Write-Host "Using the package kept from an earlier run: $cached"
            return $cached
        }
        Write-Host "The kept package no longer matches its hash. Downloading it again."
        Remove-Item -LiteralPath $cached -Force
    }
    New-Item -ItemType Directory -Force -Path $CacheDir | Out-Null
    $partial = "$cached.partial"
    Write-Host "Downloading $PackageUrl"
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
    try {
        Invoke-WebRequest -UseBasicParsing -Uri $PackageUrl -OutFile $partial
    } catch {
        if (Test-Path -LiteralPath $partial) { Remove-Item -LiteralPath $partial -Force }
        throw "Could not download the Microsoft package ($($_.Exception.Message)).`n  from     $PackageUrl`nCheck the connection and run this again, or download it by hand and pass it with -Package."
    }
    try {
        Assert-Hash $partial $PackageSha256 "The downloaded package"
    } catch {
        Remove-Item -LiteralPath $partial -Force
        throw
    }
    Move-Item -LiteralPath $partial -Destination $cached -Force
    $cached
}

# Takes the two Microsoft files out of the package into $Into and checks
# each one both ways.
function Expand-MicrosoftFiles([string]$PackagePath, [string]$Into) {
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $zip = [IO.Compression.ZipFile]::OpenRead($PackagePath)
    try {
        foreach ($file in $MicrosoftFiles) {
            $entry = $zip.GetEntry($file.Entry)
            if ($null -eq $entry) {
                throw "The package has no $($file.Entry). It is not the package this script was written for."
            }
            $target = Join-Path $Into $file.Name
            [IO.Compression.ZipFileExtensions]::ExtractToFile($entry, $target, $true)
        }
    } finally {
        $zip.Dispose()
    }
    foreach ($file in $MicrosoftFiles) {
        $target = Join-Path $Into $file.Name
        Assert-Hash $target $file.Sha256 $file.Name
        Assert-MicrosoftSignature $target $file.Name
        Write-Host "  checked  $($file.Name): hash matches, signed by $RequiredSigner"
    }
}

# Dot-sourcing this file loads the functions above without packaging anything.
if ($MyInvocation.InvocationName -eq '.') { return }

$repo    = Split-Path -Parent $PSScriptRoot
$dist    = Join-Path $repo 'dist'
$staging = Join-Path $dist ".staging-$PID"
$final   = $null

try {
    if ($env:PROCESSOR_ARCHITECTURE -ne 'AMD64') {
        throw "This script packages the x64 build only, and this machine reports $($env:PROCESSOR_ARCHITECTURE). Builds for arm64 and x86 are not set up."
    }
    $notice = Join-Path $repo "packaging\windows\$NoticeName"
    $readme = Join-Path $repo 'README.md'
    $license = Join-Path $repo 'LICENSE'
    foreach ($needed in @($notice, $readme, $license)) {
        if (-not (Test-Path -LiteralPath $needed -PathType Leaf) -or (Get-Item -LiteralPath $needed).Length -eq 0) {
            throw "A file that must ship is missing or empty: $needed"
        }
    }

    Write-Host "Building lillignode.exe (cargo build --release)"
    Push-Location $repo
    try {
        $hostLine = Invoke-Program 'rustc -vV' | Where-Object { $_ -like 'host:*' }
        if ($hostLine -notlike '*x86_64-pc-windows-*') {
            throw "The Rust toolchain here builds for `"$hostLine`", not x64 Windows. The Microsoft files packaged are x64."
        }
        # Rust writes the source paths of its dependencies into the program,
        # for its panic messages, and those paths name this machine's user
        # folder. They are rewritten to short neutral names for the release.
        # CARGO_ENCODED_RUSTFLAGS keeps a path with spaces in one piece.
        $cargoHome  = if ($env:CARGO_HOME)  { $env:CARGO_HOME }  else { Join-Path $env:USERPROFILE '.cargo' }
        $rustupHome = if ($env:RUSTUP_HOME) { $env:RUSTUP_HOME } else { Join-Path $env:USERPROFILE '.rustup' }
        $remaps = @("$cargoHome=cargo", "$rustupHome=rustup", "$repo=lillignode", "$($env:USERPROFILE)=home")
        $flagsBefore = $env:CARGO_ENCODED_RUSTFLAGS
        $env:CARGO_ENCODED_RUSTFLAGS = ($remaps | ForEach-Object { "--remap-path-prefix=$_" }) -join [char]0x1f
        # A release is never built without its icon (build.rs).
        $iconBefore = $env:LILLIGNODE_REQUIRE_ICON
        $env:LILLIGNODE_REQUIRE_ICON = '1'
        try {
            Invoke-Program 'cargo build --release --locked' | Out-Null
        } finally {
            $env:CARGO_ENCODED_RUSTFLAGS = $flagsBefore
            $env:LILLIGNODE_REQUIRE_ICON = $iconBefore
        }
    } finally {
        Pop-Location
    }
    $exe = Join-Path $repo 'target\release\lillignode.exe'
    if (-not (Test-Path -LiteralPath $exe -PathType Leaf)) {
        throw "The build finished but $exe is not there."
    }
    # Checked, not assumed: the program must not name this machine's folders.
    $bytes = [IO.File]::ReadAllBytes($exe)
    $text = [Text.Encoding]::GetEncoding(28591).GetString($bytes)
    foreach ($local in @($env:USERPROFILE, $repo, $env:USERNAME)) {
        if ($local -and $text.IndexOf($local, [StringComparison]::OrdinalIgnoreCase) -ge 0) {
            throw "The built lillignode.exe still contains a path or name from this machine ($local). Nothing was packaged."
        }
    }

    $packagePath = Get-PinnedPackage (Join-Path $repo 'target\package-cache') $Package

    # A run that was killed part way leaves its half-made folder behind.
    # Those are never release folders; clear them before making a new one.
    if (Test-Path -LiteralPath $dist) {
        Get-ChildItem -LiteralPath $dist -Directory -Force -Filter '.staging-*' |
            ForEach-Object { Remove-Item -LiteralPath $_.FullName -Recurse -Force }
    }
    New-Item -ItemType Directory -Force -Path $staging | Out-Null
    Write-Host "Checking the Microsoft files"
    Expand-MicrosoftFiles $packagePath $staging
    Copy-Item -LiteralPath $exe    -Destination (Join-Path $staging 'lillignode.exe')
    Copy-Item -LiteralPath $notice -Destination (Join-Path $staging $NoticeName)
    Copy-Item -LiteralPath $readme -Destination (Join-Path $staging 'README.md')
    Copy-Item -LiteralPath $license -Destination (Join-Path $staging 'LICENSE')

    # The packaged program must start. It is pointed at a throwaway folder so
    # it reads nobody's real workspace or settings.
    $throwaway = Join-Path ([IO.Path]::GetTempPath()) "lillignode-package-check-$PID"
    $before = $env:LILLIGNODE_STATE_DIR
    $env:LILLIGNODE_STATE_DIR = $throwaway
    try {
        $said = (Invoke-Program "`"$(Join-Path $staging 'lillignode.exe')`" --version") -join ' '
    } finally {
        $env:LILLIGNODE_STATE_DIR = $before
        if (Test-Path -LiteralPath $throwaway) { Remove-Item -LiteralPath $throwaway -Recurse -Force }
    }
    if ($said -notmatch '^Lillignode (\d+\.\d+\.\d+\S*)$') {
        throw "The packaged lillignode.exe did not report its version. It said: $said"
    }
    $version = $Matches[1]

    # Everything is checked; only now is the release folder put in place.
    $final = Join-Path $dist "lillignode-$version-windows-x64"
    if (Test-Path -LiteralPath $final) { Remove-Item -LiteralPath $final -Recurse -Force }
    Move-Item -LiteralPath $staging -Destination $final

    # Checked once more where they will ship from.
    foreach ($file in $MicrosoftFiles) {
        $shipped = Join-Path $final $file.Name
        Assert-Hash $shipped $file.Sha256 "$($file.Name) in the release folder"
        Assert-MicrosoftSignature $shipped "$($file.Name) in the release folder"
    }

    # The zip holds the release folder itself, so it unpacks into one folder.
    # Its SHA-256 is written beside it, in the form sha256sum -c reads.
    $zipPath = "$final.zip"
    $hashPath = "$zipPath.sha256"
    foreach ($old in @($zipPath, $hashPath)) {
        if (Test-Path -LiteralPath $old) { Remove-Item -LiteralPath $old -Force }
    }
    # Each entry is named with forward slashes, as the zip format requires.
    # Windows PowerShell's CreateFromDirectory writes backslashes, which
    # other systems' unzip tools do not read as folders.
    Add-Type -AssemblyName System.IO.Compression
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $folderName = Split-Path -Leaf $final
    $archive = [IO.Compression.ZipFile]::Open($zipPath, [IO.Compression.ZipArchiveMode]::Create)
    try {
        Get-ChildItem -LiteralPath $final -File | Sort-Object Name | ForEach-Object {
            [IO.Compression.ZipFileExtensions]::CreateEntryFromFile(
                $archive, $_.FullName, "$folderName/$($_.Name)", [IO.Compression.CompressionLevel]::Optimal) | Out-Null
        }
    } finally {
        $archive.Dispose()
    }
    $zipHash = Get-Sha256 $zipPath
    [IO.File]::WriteAllText($hashPath, "$zipHash  $(Split-Path -Leaf $zipPath)`n", (New-Object Text.UTF8Encoding $false))

    if ($BesideBuild) {
        foreach ($file in $MicrosoftFiles) {
            Copy-Item -LiteralPath (Join-Path $final $file.Name) -Destination (Join-Path $repo "target\release\$($file.Name)") -Force
        }
        Write-Host "Also placed conpty.dll and OpenConsole.exe beside target\release\lillignode.exe."
    }

    Write-Host ""
    Write-Host "Release folder: $final"
    Get-ChildItem -LiteralPath $final | Sort-Object Name | ForEach-Object {
        Write-Host ("  {0}  {1,9:N0} bytes  {2}" -f (Get-Sha256 $_.FullName), $_.Length, $_.Name)
    }
    Write-Host ""
    Write-Host "Zip: $zipPath"
    Write-Host "  SHA-256 $zipHash (also in $(Split-Path -Leaf $hashPath))"
    Write-Host ""
    Write-Host "Lillignode $version packaged. Both Microsoft files matched their hashes and are signed by $RequiredSigner."
    exit 0
} catch {
    if (Test-Path -LiteralPath $staging) { Remove-Item -LiteralPath $staging -Recurse -Force }
    $kept = if ($final -and (Test-Path -LiteralPath $final)) {
        "The release folder $final was already in place when this happened. Do not ship it; delete it and run this again."
    } else {
        "No release folder was written, and any release folder from an earlier run was left as it was."
    }
    [Console]::Error.WriteLine("")
    [Console]::Error.WriteLine("PACKAGING FAILED. Nothing from this run may be shipped.")
    [Console]::Error.WriteLine($_.Exception.Message)
    [Console]::Error.WriteLine($kept)
    exit 1
}

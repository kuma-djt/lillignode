<#
.SYNOPSIS
Checks that scripts\package-windows.ps1 refuses what it must refuse.

.DESCRIPTION
Builds damaged copies of the pinned Microsoft package in a throwaway folder
and confirms that each one is refused, for the right reason, and that a
refused run leaves no release folder behind. Downloads nothing: it needs the
package kept by an earlier run of package-windows.ps1.

    powershell -ExecutionPolicy Bypass -File scripts\test-package-windows.ps1
#>
. "$PSScriptRoot\package-windows.ps1"

$repo = Split-Path -Parent $PSScriptRoot
$real = Join-Path $repo "target\package-cache\$PackageName"
if (-not (Test-Path -LiteralPath $real -PathType Leaf)) {
    [Console]::Error.WriteLine("The pinned package is not in target\package-cache yet. Run scripts\package-windows.ps1 once, then this again. Nothing was checked.")
    exit 2
}
$work = Join-Path ([IO.Path]::GetTempPath()) "lillignode-package-test-$PID"
New-Item -ItemType Directory -Force -Path $work | Out-Null
$script:failed = 0

function Pass([string]$What) { Write-Host "  ok    $What" }
function Fail([string]$What, [string]$Detail) {
    Write-Host "  FAIL  $What`n        $Detail"
    $script:failed++
}

# The body must throw, and say why in the words expected.
function Expect-Refusal([string]$What, [string]$Pattern, [scriptblock]$Body) {
    try {
        & $Body
        Fail $What "it was accepted"
    } catch {
        if ($_.Exception.Message -match $Pattern) { Pass $What }
        else { Fail $What "refused, but for another reason: $($_.Exception.Message)" }
    }
}

# A copy of the real package in which one file has one byte changed, or is
# left out altogether.
function New-DamagedPackage([string]$Name, [string]$Entry, [switch]$Remove) {
    $copy = Join-Path $work $Name
    Copy-Item -LiteralPath $real -Destination $copy
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $zip = [IO.Compression.ZipFile]::Open($copy, 'Update')
    try {
        $old = $zip.GetEntry($Entry)
        $memory = New-Object IO.MemoryStream
        $stream = $old.Open(); $stream.CopyTo($memory); $stream.Dispose()
        $bytes = $memory.ToArray()
        $old.Delete()
        if (-not $Remove) {
            $middle = [int]($bytes.Length / 2)
            $bytes[$middle] = $bytes[$middle] -bxor 0x01
            $new = $zip.CreateEntry($Entry)
            $stream = $new.Open(); $stream.Write($bytes, 0, $bytes.Length); $stream.Dispose()
        }
    } finally {
        $zip.Dispose()
    }
    $copy
}

function New-Folder([string]$Name) {
    $path = Join-Path $work $Name
    New-Item -ItemType Directory -Force -Path $path | Out-Null
    $path
}

try {
    Write-Host "A. The real package"
    $good = New-Folder 'good'
    try {
        Expand-MicrosoftFiles $real $good
        Pass "both files are accepted"
    } catch {
        Fail "both files are accepted" $_.Exception.Message
    }

    Write-Host "B. A file inside the package was changed"
    foreach ($file in $MicrosoftFiles) {
        $damaged = New-DamagedPackage "changed-$($file.Name).nupkg" $file.Entry
        Expect-Refusal "$($file.Name) with one byte changed is refused by its hash" `
            "$([regex]::Escape($file.Name)) does not match its recorded SHA-256 hash" `
            { Expand-MicrosoftFiles $damaged (New-Folder "changed-$($file.Name)") }
    }
    $missing = New-DamagedPackage 'missing.nupkg' $MicrosoftFiles[1].Entry -Remove
    Expect-Refusal "a package without OpenConsole.exe is refused" 'The package has no build/native/runtimes/x64/OpenConsole.exe' `
        { Expand-MicrosoftFiles $missing (New-Folder 'missing') }

    Write-Host "C. The signature, checked on its own"
    foreach ($file in $MicrosoftFiles) {
        $changed = Join-Path $work "resigned-$($file.Name)"
        $bytes = [IO.File]::ReadAllBytes((Join-Path $good $file.Name))
        $bytes[2000] = $bytes[2000] -bxor 0x01
        [IO.File]::WriteAllBytes($changed, $bytes)
        Expect-Refusal "$($file.Name) with one byte changed fails its signature too" 'does not carry a valid signature' `
            { Assert-MicrosoftSignature $changed $file.Name }
    }
    $unsigned = Join-Path $work 'unsigned.exe'
    Copy-Item -LiteralPath (Join-Path $repo 'target\release\lillignode.exe') -Destination $unsigned
    Expect-Refusal "a program with no signature is refused" 'does not carry a valid signature \(Windows reports: NotSigned\)' `
        { Assert-MicrosoftSignature $unsigned 'unsigned.exe' }

    Write-Host "D. The whole script, given a changed package"
    $dist = Join-Path $repo 'dist'
    $snapshot = {
        if (Test-Path -LiteralPath $dist) {
            Get-ChildItem -LiteralPath $dist -Recurse -Force | ForEach-Object {
                $size = if ($_.PSIsContainer) { 'folder' } else { $_.Length }
                "$($_.FullName)|$size|$($_.LastWriteTimeUtc.Ticks)"
            }
        }
    }
    $before = (& $snapshot) -join "`n"
    $changedPackage = Join-Path $work "changed-conpty.dll.nupkg"
    $output = & cmd.exe /d /c "powershell -NoProfile -ExecutionPolicy Bypass -File `"$PSScriptRoot\package-windows.ps1`" -Package `"$changedPackage`" 2>&1"
    $code = $LASTEXITCODE
    $text = $output -join "`n"
    if ($code -eq 1) { Pass "it exits with an error code" } else { Fail "it exits with an error code" "exit code $code" }
    if ($text -match 'PACKAGING FAILED' -and $text -match 'The package given with -Package does not match its recorded SHA-256 hash') {
        Pass "it says loudly what was wrong"
    } else { Fail "it says loudly what was wrong" $text }
    if ($text -match 'No release folder was written') { Pass "it says nothing was written" }
    else { Fail "it says nothing was written" $text }
    if (((& $snapshot) -join "`n") -eq $before) { Pass "dist is exactly as it was: no new folder, nothing left half made" }
    else { Fail "dist is exactly as it was" "dist changed" }
} finally {
    Remove-Item -LiteralPath $work -Recurse -Force
}

Write-Host ""
if ($script:failed -gt 0) {
    Write-Host "$($script:failed) check(s) FAILED."
    exit 1
}
Write-Host "All packaging checks passed."
exit 0

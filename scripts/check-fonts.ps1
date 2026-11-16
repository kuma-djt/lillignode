# Checks that every character Lillignode itself draws exists in the fonts
# Windows terminals use: Consolas (Windows Terminal's fallback and the console
# window's usual font) and Lucida Console (the console window's older one).
#
#   powershell -ExecutionPolicy Bypass -File scripts\check-fonts.ps1
#
# What Lillignode draws is printable ASCII plus the characters listed in
# NOT_ASCII in src\look.rs. A unit test holds every screen to that list; this
# script holds the list to the fonts. It reads the fonts and changes nothing.
#
# It does not check an agent's own screen. Lillignode passes on whatever
# characters an agent prints, and whether those draw is up to the font.

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName PresentationCore

$root = Split-Path -Parent $PSScriptRoot
$source = [IO.File]::ReadAllText((Join-Path $root 'src\look.rs'), [Text.Encoding]::UTF8)
$list = [regex]::Match($source, "NOT_ASCII: \[char; \d+\] = \[([^\]]*)\]")
if (-not $list.Success) {
    Write-Host 'FONT CHECK FAILED: could not find NOT_ASCII in src\look.rs.'
    exit 1
}
$extra = [regex]::Matches($list.Groups[1].Value, "'(.)'") | ForEach-Object { [int][char]$_.Groups[1].Value }
$codes = @(0x20..0x7E) + @($extra)

$fonts = [ordered]@{
    'Consolas'       = 'consola.ttf'
    'Lucida Console' = 'lucon.ttf'
}
$failed = $false
foreach ($name in $fonts.Keys) {
    $path = Join-Path $env:WINDIR ('Fonts\' + $fonts[$name])
    if (-not (Test-Path $path)) {
        Write-Host "FONT CHECK FAILED: $name is not installed here ($path), so it could not be checked."
        $failed = $true
        continue
    }
    $face = New-Object Windows.Media.GlyphTypeface (New-Object Uri $path)
    $missing = @($codes | Where-Object { -not $face.CharacterToGlyphMap.ContainsKey($_) })
    if ($missing.Count -eq 0) {
        Write-Host ("  checked  {0}: all {1} characters are in it" -f $name, $codes.Count)
    } else {
        $shown = ($missing | ForEach-Object { 'U+{0:X4}' -f $_ }) -join ', '
        Write-Host "  MISSING  ${name}: $shown"
        $failed = $true
    }
}
$named = ($extra | ForEach-Object { 'U+{0:X4}' -f $_ }) -join ', '
if ($failed) {
    Write-Host 'FONT CHECK FAILED. Nothing was changed.'
    exit 1
}
Write-Host "Every character Lillignode draws is in both fonts: printable ASCII and $named."

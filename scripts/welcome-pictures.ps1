# Pictures of the first screen in a real Windows Terminal window: each of the
# three layouts ("welcome_look" in the settings) at 100, 140 and 180 columns.
# For CI on a machine that belongs to the run: it starts Lillignode with no
# agents in a throwaway state folder, waits for the screen, and saves a
# picture and the screen's text. It types nothing and starts no agent.
param(
    [Parameter(Mandatory)] [string]$Program,    # lillignode.exe, with its pseudo-terminal files beside it
    [Parameter(Mandatory)] [string]$Terminal,   # WindowsTerminal.exe
    [Parameter(Mandatory)] [string]$Out
)
$ErrorActionPreference = 'Stop'
if (-not ($env:GITHUB_ACTIONS -eq 'true' -and $env:CI -eq 'true')) {
    throw "This opens windows on the desktop it runs on; it is for CI only. Nothing was started."
}

Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes, System.Drawing
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class Win {
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
}
"@
[void][Win]::SetProcessDPIAware()
New-Item -ItemType Directory -Force $Out | Out-Null
$rows = 40
$failed = $false

function Find-Window($before) {
    $cond = New-Object Windows.Automation.PropertyCondition(
        [Windows.Automation.AutomationElement]::ClassNameProperty, 'CASCADIA_HOSTING_WINDOW_CLASS')
    foreach ($w in [Windows.Automation.AutomationElement]::RootElement.FindAll('Children', $cond)) {
        if ($before -notcontains $w.Current.ProcessId) { return $w }
    }
}
function Read-Screen($window) {
    $cond = New-Object Windows.Automation.PropertyCondition(
        [Windows.Automation.AutomationElement]::IsTextPatternAvailableProperty, $true)
    $el = $window.FindAll('Descendants', $cond) | Where-Object { $_.Current.ClassName -eq 'TermControl' } | Select-Object -First 1
    if (-not $el) { return '' }
    $tp = $el.GetCurrentPattern([Windows.Automation.TextPattern]::Pattern)
    ($tp.GetVisibleRanges() | ForEach-Object { $_.GetText(-1) }) -join "`n"
}
function Save-Picture($hwnd, [string]$path) {
    $r = New-Object Win+RECT
    [void][Win]::GetWindowRect($hwnd, [ref]$r)
    $w = $r.R - $r.L; $h = $r.B - $r.T
    $bmp = New-Object Drawing.Bitmap $w, $h
    $g = [Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen($r.L, $r.T, 0, 0, $bmp.Size)
    $bmp.Save($path); $g.Dispose(); $bmp.Dispose()
}

$n = 0
foreach ($look in 'beside', 'stacked', 'wide') {
    foreach ($cols in 100, 140, 180) {
        $n++
        $state = Join-Path $Out "state-$look-$cols"
        $cc = Join-Path $Out "cc-none-$look-$cols"
        New-Item -ItemType Directory -Force $state, $cc | Out-Null
        Set-Content -Encoding ascii (Join-Path $state 'config.json') "{ `"welcome_look`": `"$look`" }"
        $env:LILLIGNODE_STATE_DIR = $state
        $env:CLAUDE_CONFIG_DIR = $cc
        $before = @(Get-Process WindowsTerminal -ErrorAction SilentlyContinue | ForEach-Object Id)
        Start-Process $Terminal -ArgumentList "--size $cols,$rows new-tab `"$Program`""
        $window = $null
        foreach ($i in 1..60) { $window = Find-Window $before; if ($window) { break }; Start-Sleep -Milliseconds 500 }
        if (-not $window) { Write-Host "FAIL  $look $cols : no window"; $failed = $true; continue }
        $hwnd = [IntPtr]$window.Current.NativeWindowHandle
        [void][Win]::SetForegroundWindow($hwnd)
        $text = ''
        foreach ($i in 1..40) {
            $text = Read-Screen $window
            if ($text.Contains('start an agent')) { break }
            Start-Sleep -Milliseconds 250
        }
        Start-Sleep -Milliseconds 800
        $stem = Join-Path $Out ('{0:00}-{1}-{2}' -f $n, $look, $cols)
        $text | Set-Content -Encoding utf8 "$stem.txt"
        Save-Picture $hwnd "$stem.png"
        # The key, the large name, and a window as wide as asked (its widest
        # line is near the column count).
        $widest = ($text -split "`n" | ForEach-Object { $_.TrimEnd().Length } | Measure-Object -Maximum).Maximum
        $why = @()
        if (-not $text.Contains('start an agent')) { $why += 'no key on screen' }
        if (-not $text.Contains([string][char]0x2588)) { $why += 'no large name' }
        if ($widest -lt $cols - 30) { $why += "the window is narrower than asked ($widest of $cols)" }
        if ($why.Count -eq 0) { Write-Host "PASS  $look $cols" } else { Write-Host ("FAIL  $look $cols : " + ($why -join '; ')); $failed = $true }
        Get-Process lillignode -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
        Get-Process WindowsTerminal -ErrorAction SilentlyContinue | Where-Object { $before -notcontains $_.Id } | Stop-Process -Force -ErrorAction SilentlyContinue
        Start-Sleep -Seconds 1
    }
}
if ($failed) { exit 1 }

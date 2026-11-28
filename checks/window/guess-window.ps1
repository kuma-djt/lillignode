# A helper's guess in the terminal, in a real Windows Terminal window on a
# desktop that belongs to the run: Lillignode built from the branch, with
# the stand-in helper named in its settings, and one stand-in agent that
# reports nothing and shows what is typed to it. Typed "ok?": the helper
# guesses waiting; then "." changes the screen and the guess goes; still
# again, it guesses done. A picture at each step.
#
# Rules: it types only after seeing the text it expects; only into the
# window it started; throwaway folders; no real agent.
param(
    [Parameter(Mandatory)] [ValidateSet('ci')] [string]$Seat,
    [Parameter(Mandatory)] [string]$Program,
    [Parameter(Mandatory)] [string]$Standin,    # standin.exe, built beside it
    [Parameter(Mandatory)] [string]$Helper,     # helper_standin.exe, built beside it
    [Parameter(Mandatory)] [string]$Terminal,
    [Parameter(Mandatory)] [string]$Out
)
$ErrorActionPreference = 'Stop'
if (-not ($env:GITHUB_ACTIONS -eq 'true' -and $env:CI -eq 'true' -and $env:RUNNER_NAME -and $env:RUNNER_TEMP)) {
    throw "Seat 'ci' but this is not a CI runner. Not started."
}

Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes, System.Windows.Forms, System.Drawing
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class Win {
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
}
"@
[void][Win]::SetProcessDPIAware()

New-Item -ItemType Directory -Force $Out, "$Out\state", "$Out\launchers", "$Out\work", "$Out\frames", "$Out\cc-none" | Out-Null
$results = New-Object System.Collections.Generic.List[object]
$script:shot = 0
$script:frame = 0

$cols = 128
$rows = 40

$env:CLAUDE_CONFIG_DIR = "$Out\cc-none"
$basePath = $env:PATH
$before = @(Get-Process WindowsTerminal, lillignode -ErrorAction SilentlyContinue | ForEach-Object Id)

function Find-Window {
    $cond = New-Object Windows.Automation.PropertyCondition(
        [Windows.Automation.AutomationElement]::ClassNameProperty, 'CASCADIA_HOSTING_WINDOW_CLASS')
    foreach ($w in [Windows.Automation.AutomationElement]::RootElement.FindAll('Children', $cond)) {
        if ($before -notcontains $w.Current.ProcessId) { return $w }
    }
}


function Read-Screen {
    $cond = New-Object Windows.Automation.PropertyCondition(
        [Windows.Automation.AutomationElement]::IsTextPatternAvailableProperty, $true)
    $el = $window.FindAll('Descendants', $cond) | Where-Object { $_.Current.ClassName -eq 'TermControl' } | Select-Object -First 1
    if (-not $el) { return '' }
    $tp = $el.GetCurrentPattern([Windows.Automation.TextPattern]::Pattern)
    ($tp.GetVisibleRanges() | ForEach-Object { $_.GetText(-1) }) -join "`n"
}
function Save-Picture([string]$path) {
    $r = New-Object Win+RECT
    [void][Win]::GetWindowRect($hwnd, [ref]$r)
    $w = $r.R - $r.L; $h = $r.B - $r.T
    if ($w -le 0 -or $h -le 0) { return }
    $bmp = New-Object Drawing.Bitmap $w, $h
    $g = [Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen($r.L, $r.T, 0, 0, $bmp.Size)
    $bmp.Save($path); $g.Dispose(); $bmp.Dispose()
}
$script:lastFrame = [DateTime]::MinValue
function Frame {
    if (((Get-Date) - $script:lastFrame).TotalMilliseconds -lt 450) { return }
    $script:lastFrame = Get-Date
    $script:frame++
    Save-Picture ('{0}\frames\{1:0000}.png' -f $Out, $script:frame)
}
# Claude Code says its status about a second and a half before it redraws
# its own screen (as measured in real windows). A picture is taken once the screen
# has been still for a second, so it shows the moment settled.
function Wait-Still([int]$seconds = 8) {
    $end = (Get-Date).AddSeconds($seconds)
    $last = Read-Screen; $since = Get-Date
    while ((Get-Date) -lt $end) {
        Start-Sleep -Milliseconds 200; Frame
        $now = Read-Screen
        if ($now -ne $last) { $last = $now; $since = Get-Date }
        elseif (((Get-Date) - $since).TotalMilliseconds -ge 1000) { return }
    }
}
function Save-Look([string]$name) {
    if ($name -ne 'stopped' -and $name -ne 'not-in-front') { Wait-Still }
    $script:shot++
    $stem = '{0}\{1:00}-{2}' -f $Out, $script:shot, $name
    Read-Screen | Set-Content -Encoding utf8 "$stem.txt"
    Save-Picture "$stem.png"
}
function Wait-Text([string]$text, [int]$seconds = 30) {
    $end = (Get-Date).AddSeconds($seconds)
    do { Frame; $s = Read-Screen; if ($s.Contains($text)) { return $s }; Start-Sleep -Milliseconds 150 } while ((Get-Date) -lt $end)
    Save-Look 'stopped'
    throw "Stopped: did not see `"$text`" in $seconds seconds. Nothing more was typed."
}
# The agent's row in the sidebar: the line of the sidebar that names its folder.
function Row([string]$screen) {
    $bar = [string][char]0x2502
    $side = ($screen -split "`r?`n") | ForEach-Object { ($_ -split [regex]::Escape($bar))[0] } | Where-Object { $_ -match '^\s*work\s' }
    ($side | Select-Object -First 1) -as [string]
}
function Wait-Row([string]$words, [int]$seconds = 40) {
    $end = (Get-Date).AddSeconds($seconds)
    do { Frame; $s = Read-Screen; if ((Row $s) -match [regex]::Escape($words)) { return $s }; Start-Sleep -Milliseconds 100 } while ((Get-Date) -lt $end)
    Save-Look 'stopped'
    throw "Stopped: the row says `"$(Row $s)`", expected $words. Nothing more was typed."
}
function Step([string]$name, [bool]$ok, [string]$detail) {
    $results.Add([pscustomobject]@{ step = $name; ok = $ok; detail = $detail })
    Write-Host ("{0}  {1}  {2}" -f $(if ($ok) { 'PASS' } else { 'FAIL' }), $name, $detail)
}
function In-Front {
    if ([Win]::GetForegroundWindow() -ne $hwnd) { [void][Win]::SetForegroundWindow($hwnd); Start-Sleep -Milliseconds 300 }
    if ([Win]::GetForegroundWindow() -ne $hwnd) { Save-Look 'not-in-front'; throw "Stopped: the window in front is not the one this run started. Nothing was typed." }
}
function Press([string]$keys) { In-Front; [Windows.Forms.SendKeys]::SendWait($keys); Frame; Start-Sleep -Milliseconds 300 }
# Typed one key at a time, as a person types, so no burst is taken for a paste.
function Type-Line([string]$text) {
    foreach ($c in $text.ToCharArray()) {
        $k = if ('+^%~(){}[]'.Contains($c)) { "{$c}" } else { [string]$c }
        In-Front; [Windows.Forms.SendKeys]::SendWait($k); Start-Sleep -Milliseconds 25
    }
    Frame; Start-Sleep -Milliseconds 300
    Press '{ENTER}'
}

function Open-Lillignode([string]$state) {
    New-Item -ItemType Directory -Force $state | Out-Null
    $env:LILLIGNODE_STATE_DIR = $state
    Start-Process $Terminal -ArgumentList "--size $cols,$rows new-tab `"$Program`""
    $script:window = $null
    foreach ($i in 1..60) { $script:window = Find-Window; if ($script:window) { break }; Start-Sleep -Milliseconds 500 }
    if (-not $script:window) { throw "No Windows Terminal window appeared in 30 seconds." }
    $script:hwnd = [IntPtr]$script:window.Current.NativeWindowHandle
}
function Close-Lillignode {
    Press '{F12}'; Press 'q'
    foreach ($i in 1..40) { if (-not (Get-Process lillignode -ErrorAction SilentlyContinue | Where-Object { $before -notcontains $_.Id })) { break }; Start-Sleep -Milliseconds 250 }
    Get-Process WindowsTerminal -ErrorAction SilentlyContinue | Where-Object { $before -notcontains $_.Id } | Stop-Process -Force -ErrorAction SilentlyContinue
    Start-Sleep -Seconds 1
}
try {
    New-Item -ItemType Directory -Force "$Out\state-g", "$Out\work" | Out-Null
    $ws = @{ version = 1; focused = 0; agents = @(
        @{ name = 'plain'; command = ($Standin -replace '\\', '/'); kind = 'program'; args = @('echo-plain'); folder = ("$Out\work" -replace '\\', '/') }) }
    $ws | ConvertTo-Json -Depth 5 | Set-Content -Encoding ascii "$Out\state-g\workspace.json"
    $named = @{ helper = @{ command = ($Helper -replace '\\', '/'); args = @('read') } }
    $named | ConvertTo-Json -Depth 5 | Set-Content -Encoding ascii "$Out\state-g\config.json"
    $env:PATH = $basePath
    Open-Lillignode "$Out\state-g"
    $s = Wait-Text 'STANDIN READY echo-plain' 40
    Step 'the stand-in starts' $true ''
    $s = Wait-Text 'is on: it is sent the screen' 10
    Save-Look 'helper-on'
    Step 'the start names the helper and says what it is sent' ($s.Contains('helper_standin') -and $s.Contains('any agent that reports nothing')) ''
    foreach ($c in 'ok?'.ToCharArray()) { Press ([string]$c) }
    $s = Wait-Text 'Unknown (waiting?)' 10
    Save-Look 'guess'
    Step 'after two seconds still, the sidebar shows the guess, marked as one' $s.Contains('work  Unknown (waiting?)') ''
    Press '{F12}'; Press 's'
    $s = Wait-Text 'guess, not the agent' 10
    Save-Look 'status-report'
    Step 'the status report says it is a guess, not the agent''s report' $true ''
    Press '{ESC}'
    Press '.'
    Start-Sleep -Milliseconds 500
    $s = Read-Screen
    Step 'a changed screen ends the guess' (-not $s.Contains('Unknown (waiting?)')) ''
    $s = Wait-Text 'Unknown (done?)' 10
    Save-Look 'guess-again'
    Step 'still again, it is asked again' $true ''
    Close-Lillignode
}
catch { Step 'run' $false $_.Exception.Message }
finally {
    $results | ConvertTo-Json | Set-Content -Encoding utf8 "$Out\results.json"
    Get-Process lillignode, WindowsTerminal -ErrorAction SilentlyContinue | Where-Object { $before -notcontains $_.Id } | Stop-Process -Force -ErrorAction SilentlyContinue
}
if ($results | Where-Object { -not $_.ok }) { exit 1 }

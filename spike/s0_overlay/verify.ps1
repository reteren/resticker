# S0 spike verification harness (throwaway, like the spike itself).
# Phases: baseline | overlay | click | mask | drag
param([Parameter(Mandatory=$true)][string]$Phase)

$ErrorActionPreference = 'Stop'

Add-Type @"
using System;
using System.Runtime.InteropServices;
public class U32 {
    [DllImport("user32.dll")] public static extern IntPtr FindWindow(string cls, string title);
    [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int cx, int cy, uint flags);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern IntPtr GetShellWindow();
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] public static extern void mouse_event(uint flags, uint dx, uint dy, uint data, UIntPtr extra);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern long GetWindowLongPtrW(IntPtr h, int idx);
    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr l);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassNameW(IntPtr h, System.Text.StringBuilder sb, int max);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
    [DllImport("user32.dll")] public static extern bool GetWindowPlacement(IntPtr h, ref WINDOWPLACEMENT wp);
    [DllImport("user32.dll")] public static extern bool SetWindowPlacement(IntPtr h, ref WINDOWPLACEMENT wp);
    public delegate bool EnumProc(IntPtr h, IntPtr l);
}
[StructLayout(LayoutKind.Sequential)]
public struct WINDOWPLACEMENT {
    public int length; public int flags; public int showCmd;
    public int ptMinX, ptMinY, ptMaxX, ptMaxY;
    public int normLeft, normTop, normRight, normBottom;
}
public struct RECT { public int Left, Top, Right, Bottom; }
"@
[U32]::SetProcessDPIAware() | Out-Null  # this script works in physical pixels
Add-Type -AssemblyName System.Drawing

$PNG_W = 420; $PNG_H = 300
$BASELINE_FILE = "$PSScriptRoot\baseline.clixml"

function Find-WindowByClass([string]$ClassName) {
    # note: FindWindow is unreliable on this Win11 build (returns 0 for several
    # real window classes: s0_overlay, Progman, CabinetWClass); EnumWindows works.
    $script:fwResult = [IntPtr]::Zero
    [U32]::EnumWindows({ param($h, $l)
        $cls = New-Object System.Text.StringBuilder 256
        [U32]::GetClassNameW($h, $cls, 256) | Out-Null
        if ($cls.ToString() -eq $ClassName) { $script:fwResult = $h; return $false }
        return $true
    }, [IntPtr]::Zero) | Out-Null
    return $script:fwResult
}

function Get-Overlay {
    $h = Find-WindowByClass 's0_overlay'
    if ($h -eq [IntPtr]::Zero) { throw 'overlay window not found (is the app running?)' }
    # the environment (display re-enumeration?) once moved the window to the
    # left monitor at (-1920,0); the probes all watch the primary at (0,0).
    $r = New-Object RECT
    [U32]::GetWindowRect($h, [ref]$r) | Out-Null
    if ($r.Left -ne 0 -or $r.Top -ne 0) {
        "note: overlay was at ($($r.Left),$($r.Top)) - moving back to (0,0)"
        [U32]::SetWindowPos($h, [IntPtr]::Zero, 0, 0, 0, 0, 0x5) | Out-Null  # NOSIZE|NOZORDER
        Start-Sleep -Milliseconds 300
    }
    return $h
}

function Get-PngRect {
    $h = Get-Overlay
    $r = New-Object RECT
    [U32]::GetWindowRect($h, [ref]$r) | Out-Null
    $sw = $r.Right - $r.Left; $sh = $r.Bottom - $r.Top
    return @{ X = [int](($sw - $PNG_W) / 2); Y = [int](($sh - $PNG_H) / 2)
              SW = $sw; SH = $sh; Cx = [int]($sw / 2); Cy = [int]($sh / 2) }
}

function Get-Pixel([int]$x, [int]$y) {
    $b = New-Object System.Drawing.Bitmap 1, 1
    $g = [System.Drawing.Graphics]::FromImage($b)
    $g.CopyFromScreen($x, $y, 0, 0, (New-Object System.Drawing.Size 1, 1))
    $c = $b.GetPixel(0, 0)
    $g.Dispose(); $b.Dispose()
    return $c
}

function Invoke-Click([int]$x, [int]$y) {
    [U32]::SetCursorPos($x, $y) | Out-Null
    Start-Sleep -Milliseconds 60
    [U32]::mouse_event(0x0002, 0, 0, 0, [UIntPtr]::Zero)  # LEFTDOWN
    Start-Sleep -Milliseconds 60
    [U32]::mouse_event(0x0004, 0, 0, 0, [UIntPtr]::Zero)  # LEFTUP
    Start-Sleep -Milliseconds 250
}

function Get-NotepadHwnd {
    $p = Get-Process notepad -ErrorAction SilentlyContinue | Where-Object { $_.MainWindowHandle -ne 0 } | Select-Object -First 1
    if ($p) { return $p.MainWindowHandle } else { return [IntPtr]::Zero }
}

function Save-Placement([IntPtr]$h) {
    $wp = New-Object WINDOWPLACEMENT
    $wp.length = 44
    [U32]::GetWindowPlacement($h, [ref]$wp) | Out-Null
    return $wp
}

function Restore-Placement([IntPtr]$h, $wp) {
    $wp.length = 44
    [U32]::SetWindowPlacement($h, [ref]$wp) | Out-Null
}

function Test-Orange($c) { return ($c.R -eq 255 -and $c.G -eq 128 -and $c.B -eq 0) }

switch ($Phase) {

'baseline' {
    # capture reference pixels BEFORE the overlay app is started
    $h = [U32]::FindWindow('s0_overlay', $null)
    if ($h -ne [IntPtr]::Zero) { throw 'overlay already running; close it first' }
    Add-Type -AssemblyName System.Windows.Forms
    $b = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
    $cx = [int]($b.Width / 2); $cy = [int]($b.Height / 2)
    # gradient probe point: 40px in from PNG left edge, vertically centered
    $gx = $cx - [int]($PNG_W / 2) + 40; $gy = $cy
    $data = @{ Disc = (Get-Pixel $cx $cy); Grad = (Get-Pixel $gx $gy) }
    $data | Export-Clixml $BASELINE_FILE
    "baseline: disc=$($data.Disc) grad=$($data.Grad) at ($cx,$cy)"
}

'overlay' {
    # steps 2-4: window styles, composition content, PNG with alpha
    $h = Get-Overlay
    $ex = [U32]::GetWindowLongPtrW($h, -20)  # GWL_EXSTYLE
    $TOPMOST = 0x8; $TRANSP = 0x20; $NOREDIR = 0x200000; $LAYERED = 0x80000
    $okStyle = ($ex -band $TOPMOST) -and ($ex -band $TRANSP) -and ($ex -band $NOREDIR) -and -not ($ex -band $LAYERED)
    "exstyle=0x{0:X8} topmost={1} transparent={2} noredirectionbitmap={3} layered_absent={4}" -f `
        $ex, [bool]($ex -band $TOPMOST), [bool]($ex -band $TRANSP), [bool]($ex -band $NOREDIR), -not [bool]($ex -band $LAYERED)

    $pr = Get-PngRect
    $disc = Get-Pixel $pr.Cx $pr.Cy
    $grad = Get-Pixel ($pr.X + 40) $pr.Cy
    "disc pixel = $disc (expect opaque orange R=255 G=128 B=0)"
    "grad pixel = $grad"
    $discOk = ($disc.R -eq 255 -and $disc.G -eq 128 -and $disc.B -eq 0)

    # premultiplied-alpha check against a same-row reference pixel just
    # OUTSIDE the PNG (robust to whatever is animating underneath).
    # PNG texel at probe (40,150): straight src (24,30,127,a=110) ->
    # premultiplied (10,13,55); screen = bg*(1-110/255) + premult
    $ref = Get-Pixel ($pr.X - 25) $pr.Cy
    $keep = 145.0 / 255.0
    $pr_ = [math]::Round($ref.R * $keep + 10)
    $pg_ = [math]::Round($ref.G * $keep + 13)
    $pb_ = [math]::Round($ref.B * $keep + 55)
    $alphaOk = ([math]::Abs($grad.R - $pr_) -le 10 -and [math]::Abs($grad.G - $pg_) -le 10 -and [math]::Abs($grad.B - $pb_) -le 10)
    "ref outside=($($ref.R),$($ref.G),$($ref.B)) predicted=($pr_,$pg_,$pb_) actual=($($grad.R),$($grad.G),$($grad.B)) blendmatch=$alphaOk"
    if ($okStyle -and $discOk -and $alphaOk) { 'OVERLAY: PASS' } else { 'OVERLAY: FAIL' }
}

'click' {
    # step 5: clicks pass through the overlay (even through opaque drawn pixels).
    # Target: an Explorer folder window parked directly under the PNG disc,
    # at the top of the NORMAL band (our overlay stays above it in the topmost band).
    Get-Overlay | Out-Null
    $pr = Get-PngRect

    # get the user's notepad out of the way (it may cover the click point)
    $np = Get-NotepadHwnd
    $npwp = $null
    if ($np -ne [IntPtr]::Zero) {
        $npwp = Save-Placement $np
        [U32]::ShowWindow($np, 6) | Out-Null  # SW_MINIMIZE
        Start-Sleep -Milliseconds 700
    }

    try {
        $target = Find-WindowByClass 'CabinetWClass'
        if ($target -eq [IntPtr]::Zero) {
            Start-Process explorer.exe -ArgumentList 'C:\'
            $sw = [Diagnostics.Stopwatch]::StartNew()
            while ($target -eq [IntPtr]::Zero -and $sw.Elapsed.TotalSeconds -lt 10) {
                Start-Sleep -Milliseconds 300
                $target = Find-WindowByClass 'CabinetWClass'
            }
        }
        if ($target -eq [IntPtr]::Zero) { throw 'no explorer folder window available' }

        # park target centered under the (opaque) disc, top of the normal band
        $rc = New-Object RECT; [U32]::GetWindowRect($target, [ref]$rc) | Out-Null
        $twp = Save-Placement $target
        $ww = $rc.Right - $rc.Left; $wh = $rc.Bottom - $rc.Top
        [U32]::SetWindowPos($target, [IntPtr]::Zero, $pr.Cx - [int]($ww/2), $pr.Cy - [int]($wh/2), 0, 0, 0x41) | Out-Null # HWND_TOP|NOSIZE|SHOWWINDOW
        Start-Sleep -Milliseconds 500

        # move focus elsewhere first: real click (unlocks foreground), then
        # explicitly foreground the shell desktop window
        Invoke-Click ($pr.SW - 100) ($pr.SH - 150)
        [U32]::SetForegroundWindow([U32]::GetShellWindow()) | Out-Null
        Start-Sleep -Milliseconds 400
        $before = [U32]::GetForegroundWindow()
        "foreground before = $before (target = $target)"

        Invoke-Click $pr.Cx $pr.Cy
        $after = [U32]::GetForegroundWindow()
        "foreground after  = $after"

        # restore target window placement
        Restore-Placement $target $twp

        if ($after -eq $target -and $before -ne $target) { 'CLICK-THROUGH: PASS' } else { 'CLICK-THROUGH: FAIL' }
    } finally {
        if ($npwp) { Restore-Placement $np $npwp }
    }
}

'mask' {
    # step 7: PNG must visually go UNDER Notepad (rect cut out of the render).
    # Non-destructive: saves notepad's placement, restores it at the end.
    Get-Overlay | Out-Null
    $pr = Get-PngRect

    $np = Get-NotepadHwnd
    if ($np -eq [IntPtr]::Zero) {
        Start-Process notepad.exe
        $sw = [Diagnostics.Stopwatch]::StartNew()
        while ($np -eq [IntPtr]::Zero -and $sw.Elapsed.TotalSeconds -lt 15) {
            Start-Sleep -Milliseconds 400
            $np = Get-NotepadHwnd
        }
    }
    if ($np -eq [IntPtr]::Zero) { throw 'could not get a notepad window' }
    $npwp = Save-Placement $np

    try {
        # A) notepad covering the disc center: disc must HIDE (notepad pixels, not orange)
        [U32]::ShowWindow($np, 9) | Out-Null  # SW_RESTORE (it may be minimized)
        Start-Sleep -Milliseconds 300
        [U32]::SetWindowPos($np, [IntPtr]::Zero, $pr.Cx - 200, $pr.Cy - 150, 400, 300, 0x40) | Out-Null
        Start-Sleep -Milliseconds 900   # winevent -> mask update -> render
        $under = Get-Pixel $pr.Cx $pr.Cy
        "A: disc with notepad over it = $under (expect NOT orange)"
        $hiddenOk = -not (Test-Orange $under)

        # B) PNG area NOT covered by notepad must still show the PNG (only covered part is cut)
        $side = Get-Pixel ($pr.X + 5) $pr.Cy
        $ref  = Get-Pixel ($pr.X - 25) $pr.Cy
        $sideOk = -not ([math]::Abs($side.R - $ref.R) -le 10 -and [math]::Abs($side.G - $ref.G) -le 10 -and [math]::Abs($side.B - $ref.B) -le 10)
        "B: png edge pixel = $side vs outside ref = $ref (expect different -> png drawn there)"

        # C) move notepad away: LOCATIONCHANGE fires, mask must follow, PNG whole again
        # (minimizing is out of scope for the spike: MINIMIZESTART is not in our hook set)
        [U32]::SetWindowPos($np, [IntPtr]::Zero, 40, 200, 400, 300, 0) | Out-Null
        Start-Sleep -Milliseconds 900
        $back = Get-Pixel $pr.Cx $pr.Cy
        "C: disc after notepad moved away = $back (expect orange again)"
        $backOk = Test-Orange $back
    } finally {
        Restore-Placement $np $npwp
    }

    if ($hiddenOk -and $sideOk -and $backOk) { 'MASK: PASS' } else { 'MASK: FAIL' }
}

'drag' {
    # step 8: drag notepad around for ~10s; the app measures its own CPU meanwhile.
    # Placement saved and restored so the user's window ends up untouched.
    Get-Overlay | Out-Null
    $np = Get-NotepadHwnd
    if ($np -eq [IntPtr]::Zero) { throw 'run the mask phase first (needs notepad)' }
    $pr = Get-PngRect
    $npwp = Save-Placement $np

    try {
        # normalize + size the window for dragging
        [U32]::ShowWindow($np, 9) | Out-Null  # SW_RESTORE (it may be minimized)
        Start-Sleep -Milliseconds 300
        [U32]::SetWindowPos($np, [IntPtr]::Zero, $pr.Cx - 200, $pr.Cy - 150, 400, 300, 0x40) | Out-Null
        Start-Sleep -Milliseconds 500

        "dragging notepad for 10s..."
        $t0 = Get-Date
        $sw = [Diagnostics.Stopwatch]::StartNew()
        $i = 0
        while ($sw.Elapsed.TotalSeconds -lt 10) {
            $x = $pr.Cx - 400 + [int](300 * [math]::Sin($i / 8.0))
            $y = $pr.Cy - 150 + [int](80 * [math]::Cos($i / 5.0))
            [U32]::SetWindowPos($np, [IntPtr]::Zero, $x, $y, 0, 0, 1) | Out-Null
            $i++
            Start-Sleep -Milliseconds 16
        }
        "drag done: $i moves in $([math]::Round($sw.Elapsed.TotalSeconds,1))s"
        "window: $($t0.ToString('HH:mm:ss')) .. $((Get-Date).ToString('HH:mm:ss'))"
    } finally {
        Restore-Placement $np $npwp
    }
}
}
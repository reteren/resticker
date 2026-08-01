# shot.ps1 <x> <y> <w> <h> <outfile> — capture a screen region (physical px)
param([int]$X, [int]$Y, [int]$W, [int]$H, [string]$Out)
Add-Type @"
using System;
using System.Runtime.InteropServices;
public class DpiHelper { [DllImport("user32.dll")] public static extern bool SetProcessDPIAware(); }
"@
[DpiHelper]::SetProcessDPIAware() | Out-Null
Add-Type -AssemblyName System.Drawing
$b = New-Object System.Drawing.Bitmap $W, $H
$g = [System.Drawing.Graphics]::FromImage($b)
$g.CopyFromScreen($X, $Y, 0, 0, $b.Size)
$b.Save($Out)
$g.Dispose(); $b.Dispose()
"saved $Out ($W x $H from $X,$Y)"

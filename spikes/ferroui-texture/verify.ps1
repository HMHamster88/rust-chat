# Capture the texture-spike window and sample pixels, so the GPU path is verified visually
# without a human: the colour bars, the moving block and the changing scan line must differ
# between two captures.

param(
    [string]$Exe = ".\target\release\ferroui-texture-spike.exe",
    [int]$WarmupSeconds = 4,
    [int]$Samples = 8
)

$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Drawing
Add-Type @"
using System;
using System.Runtime.InteropServices;
public class SpikeWin32 {
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr hWnd, int nCmdShow);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hWnd, out RECT lpRect);
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left; public int Top; public int Right; public int Bottom; }
}
"@

$env:FERROUI_SMOKE_EXIT_MS = "20000"
$process = Start-Process -FilePath $Exe -PassThru
try {
    Start-Sleep -Seconds $WarmupSeconds
    $process.Refresh()
    $handle = $process.MainWindowHandle
    if ($handle -eq [IntPtr]::Zero) { throw "the spike has no main window" }
    [void][SpikeWin32]::ShowWindow($handle, 9)
    [void][SpikeWin32]::SetForegroundWindow($handle)
    Start-Sleep -Milliseconds 800

    $rect = New-Object SpikeWin32+RECT
    if (-not [SpikeWin32]::GetWindowRect($handle, [ref]$rect)) { throw "GetWindowRect failed" }
    $width = $rect.Right - $rect.Left
    $height = $rect.Bottom - $rect.Top
    "window ${width}x${height} at $($rect.Left),$($rect.Top)"

    function Capture() {
        $bitmap = New-Object System.Drawing.Bitmap($width, $height)
        $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
        $graphics.CopyFromScreen($rect.Left, $rect.Top, 0, 0, $bitmap.Size)
        $graphics.Dispose()
        $bitmap
    }

    $first = Capture
    Start-Sleep -Milliseconds 900
    $second = Capture

    $y = [int]($height * 0.45)
    $changed = 0
    $lines = @()
    for ($i = 0; $i -lt $Samples; $i++) {
        $x = [int]($width * ($i + 0.5) / $Samples)
        $a = $first.GetPixel($x, $y)
        $b = $second.GetPixel($x, $y)
        $lines += ("x={0,4} first=#{1:X2}{2:X2}{3:X2} second=#{4:X2}{5:X2}{6:X2}" -f $x, $a.R, $a.G, $a.B, $b.R, $b.G, $b.B)
    }
    $lines

    # The moving block and scan line cross the presenter at different rows; compare a grid.
    $gridChanged = 0
    $gridTotal = 0
    for ($gy = 0; $gy -lt 12; $gy++) {
        for ($gx = 0; $gx -lt 20; $gx++) {
            $px = [int]($width * ($gx + 0.5) / 20)
            $py = [int]($height * ($gy + 0.5) / 12)
            $gridTotal++
            if ($first.GetPixel($px, $py).ToArgb() -ne $second.GetPixel($px, $py).ToArgb()) {
                $gridChanged++
            }
        }
    }

    $distinct = @{}
    for ($i = 0; $i -lt $Samples; $i++) {
        $x = [int]($width * ($i + 0.5) / $Samples)
        $distinct[$first.GetPixel($x, $y).ToArgb()] = $true
    }
    "sampled colours: $($distinct.Count) distinct; changed on the row: $changed of $Samples; changed on the grid: $gridChanged of $gridTotal"

    $first.Dispose()
    $second.Dispose()

    if ($distinct.Count -lt 4) { throw "the window does not show the colour-bar pattern" }
    if ($gridChanged -eq 0) { throw "the window is static; no frames were presented" }
    "TEXTURE-VISUAL OK"
}
finally {
    Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue
}

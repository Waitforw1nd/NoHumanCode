param([string]$Root, [int]$Observer, [long]$ObserverCreated, [switch]$WaitParent, [switch]$Flood)
$ErrorActionPreference = 'Stop'
Set-Location -LiteralPath $Root
$env:TEMP = $Root
$env:TMP = $Root
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class LifecycleHandles {
    [DllImport("kernel32.dll", SetLastError=true)] public static extern IntPtr GetCurrentProcess();
    [DllImport("kernel32.dll", SetLastError=true)] public static extern bool DuplicateHandle(IntPtr from, IntPtr source, IntPtr into, out IntPtr copy, uint access, bool inherit, uint flags);
    public static long Copy(IntPtr source, IntPtr observer) {
        IntPtr copy;
        if (!DuplicateHandle(GetCurrentProcess(), source, observer, out copy, 0, false, 2))
            throw new System.ComponentModel.Win32Exception();
        return copy.ToInt64();
    }
}
'@
# Only the observer's explicitly supplied identity is opened. Target process
# handles below come from the actual creation object, never a PID lookup.
$watcher = [Diagnostics.Process]::GetProcessById($Observer)
if ($watcher.StartTime.ToUniversalTime().ToFileTimeUtc() -ne $ObserverCreated) {
    throw 'observer creation identity changed'
}
$watcherHandle = $watcher.Handle
$parentHandle = [LifecycleHandles]::Copy([LifecycleHandles]::GetCurrentProcess(), $watcherHandle)
$childScript = @'
param([string]$Root)
$ErrorActionPreference='Stop'
Set-Location -LiteralPath $Root
[IO.File]::WriteAllText((Join-Path $Root 'descendant-ready'), 'ready')
$end=[DateTime]::UtcNow.AddSeconds(45)
while (!(Test-Path -LiteralPath (Join-Path $Root 'release'))) {
    if ([DateTime]::UtcNow -ge $end) { exit 98 }
    Start-Sleep -Milliseconds 10
}
exit 0
'@
[IO.File]::WriteAllText((Join-Path $Root 'child.ps1'), $childScript)
$start = [Diagnostics.ProcessStartInfo]::new((Join-Path $PSHOME 'pwsh.exe'))
$start.UseShellExecute = $false
$start.CreateNoWindow = $true
$start.RedirectStandardInput = $true
# The descendant deliberately does not inherit either Host output pipe.
$start.RedirectStandardOutput = $true
$start.RedirectStandardError = $true
foreach ($arg in @('-NoProfile','-NonInteractive','-File',(Join-Path $Root 'child.ps1'),'-Root',$Root)) {
    $start.ArgumentList.Add($arg)
}
$child = [Diagnostics.Process]::Start($start)
$child.StandardInput.Close()
$childHandle = [LifecycleHandles]::Copy($child.Handle, $watcherHandle)
@{ parent = $parentHandle; descendant = $childHandle } | ConvertTo-Json -Compress |
    Set-Content -LiteralPath (Join-Path $Root 'handles-pending.json') -Encoding utf8
Move-Item -LiteralPath (Join-Path $Root 'handles-pending.json') -Destination (Join-Path $Root 'handles.json')
$end = [DateTime]::UtcNow.AddSeconds(45)
while (!(Test-Path -LiteralPath (Join-Path $Root 'ack'))) {
    if ([DateTime]::UtcNow -ge $end) { exit 97 }
    Start-Sleep -Milliseconds 10
}
if ($Flood) {
    [Console]::Out.Write(('o' * 131072))
    [Console]::Error.Write(('e' * 131072))
    [IO.File]::WriteAllText((Join-Path $Root 'flood-finished'), 'done')
}
if ($WaitParent) {
    while (!(Test-Path -LiteralPath (Join-Path $Root 'release'))) {
        if ([DateTime]::UtcNow -ge $end) { exit 96 }
        Start-Sleep -Milliseconds 10
    }
}
exit 0

# Automated, reproducible test of the Dezh console under QEMU.
#
# Boots dezh-boot in qemu-system-riscv64, feeds a scripted sequence of commands
# into the UART (throttled, like typing), drains output concurrently so QEMU
# never blocks on a full stdout pipe, and prints the console transcript.
#
# Interactive use instead (type yourself, exit with the `halt` command):
#   qemu-system-riscv64 -machine virt -nographic -bios default `
#       -kernel target\riscv64gc-unknown-none-elf\debug\dezh-boot
#
# Usage: pwsh dezh-boot\scripts\console-test.ps1

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent (Split-Path -Parent $PSCommandPath)
Set-Location $root

$qemu = "C:\Program Files\qemu\qemu-system-riscv64.exe"
$elf = "target\riscv64gc-unknown-none-elf\debug\dezh-boot"

& cargo build | Out-Host

$psi = New-Object System.Diagnostics.ProcessStartInfo
$psi.FileName = $qemu
$psi.Arguments = "-machine virt -nographic -bios default -kernel `"$elf`""
$psi.WorkingDirectory = (Get-Location).Path
$psi.RedirectStandardInput = $true
$psi.RedirectStandardOutput = $true
$psi.UseShellExecute = $false

$p = [System.Diagnostics.Process]::Start($psi)
$sb = New-Object System.Text.StringBuilder
$ev = Register-ObjectEvent -InputObject $p -EventName OutputDataReceived -MessageData $sb -Action {
    if ($EventArgs.Data -ne $null) { [void]$Event.MessageData.AppendLine($EventArgs.Data) }
}
$p.BeginOutputReadLine()
Start-Sleep -Milliseconds 700

$commands = @('help', 'caps', 'mem', 'services', 'echo hello dezh', 'secret', 'run', 'rogue', 'uptime', 'halt')
foreach ($c in $commands) {
    $p.StandardInput.Write($c + "`n")
    $p.StandardInput.Flush()
    Start-Sleep -Milliseconds 250
}

if (-not $p.WaitForExit(8000)) { $p.Kill(); Write-Output '(timed out, killed)' }
Start-Sleep -Milliseconds 200
Unregister-Event -SourceIdentifier $ev.Name

$sb.ToString()
Write-Output "=== qemu exit code: $($p.ExitCode) (0 = clean halt) ==="

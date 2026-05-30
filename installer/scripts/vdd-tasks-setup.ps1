# Registers the EternalMonitor virtual-display enable/disable scheduled tasks and leaves
# the device DISABLED by default. Run elevated by the installer.
#
# The tasks run as SYSTEM with highest privileges, so the (non-elevated) EternalMonitor host
# can flip the virtual display on/off via `schtasks /Run` without a UAC prompt. The task
# names must match host/src/vdd.rs.

$ErrorActionPreference = 'Stop'

# Resolve the virtual display's device instance id (more robust than hard-coding 0000).
$dev = (Get-PnpDevice -Class Display -FriendlyName 'Virtual Display Driver' -ErrorAction SilentlyContinue |
    Select-Object -First 1).InstanceId
if (-not $dev) { $dev = 'ROOT\DISPLAY\0000' }

$principal = New-ScheduledTaskPrincipal -UserId 'SYSTEM' -LogonType ServiceAccount -RunLevel Highest
$settings  = New-ScheduledTaskSettingsSet -StartWhenAvailable -ExecutionTimeLimit (New-TimeSpan -Minutes 2)

function Register-VddTask([string]$Name, [string]$Argument) {
    $action = New-ScheduledTaskAction -Execute 'pnputil.exe' -Argument $Argument
    Register-ScheduledTask -TaskName $Name -Action $action -Principal $principal -Settings $settings -Force | Out-Null
}

Register-VddTask 'EternalMonitor VDD Enable'  "/enable-device `"$dev`""
Register-VddTask 'EternalMonitor VDD Disable' "/disable-device `"$dev`""

# Off by default — the host turns it on only while streaming to it.
& pnputil.exe /disable-device "$dev" | Out-Null

Write-Output "Registered EternalMonitor VDD tasks for device '$dev' (left disabled)."

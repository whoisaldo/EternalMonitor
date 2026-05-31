# Registers the EternalMonitor virtual-display enable/disable scheduled tasks and leaves the
# device DISABLED by default. Run elevated by the installer.
#
# The tasks run as SYSTEM with highest privileges, so the (non-elevated) EternalMonitor host
# can flip the virtual display on/off via `schtasks /Run` without a UAC prompt. The task names
# must match host/src/vdd.rs.
#
# Each task invokes vdd-toggle.ps1, which resolves the VDD device at TRIGGER time. We deliberately
# do NOT bake a device instance id into the tasks here — baking a guessed id (the old
# ROOT\DISPLAY\0000 fallback) breaks whenever the driver enumerates under a different id or
# reports a different friendly name.

$ErrorActionPreference = 'Stop'

$toggle = Join-Path $PSScriptRoot 'vdd-toggle.ps1'
if (-not (Test-Path $toggle)) {
    Write-Error "vdd-toggle.ps1 not found next to this script ($toggle); cannot register VDD tasks."
    exit 1
}

$principal = New-ScheduledTaskPrincipal -UserId 'SYSTEM' -LogonType ServiceAccount -RunLevel Highest
$settings  = New-ScheduledTaskSettingsSet -StartWhenAvailable -ExecutionTimeLimit (New-TimeSpan -Minutes 2)

function Register-VddTask([string]$Name, [string]$ToggleAction) {
    $argument = "-NoProfile -ExecutionPolicy Bypass -File `"$toggle`" -Action $ToggleAction"
    $action = New-ScheduledTaskAction -Execute 'powershell.exe' -Argument $argument
    Register-ScheduledTask -TaskName $Name -Action $action -Principal $principal -Settings $settings -Force | Out-Null
}

Register-VddTask 'EternalMonitor VDD Enable'  'enable'
Register-VddTask 'EternalMonitor VDD Disable' 'disable'

# Verify the tasks actually registered — fail the install loudly if not, instead of silently
# leaving the host unable to control the virtual display at runtime.
foreach ($name in 'EternalMonitor VDD Enable', 'EternalMonitor VDD Disable') {
    if (-not (Get-ScheduledTask -TaskName $name -ErrorAction SilentlyContinue)) {
        Write-Error "Failed to register scheduled task '$name'."
        exit 1
    }
}

# Off by default — the host turns it on only while streaming to it (and only once an iPad
# connects). Resolve + disable via the same toggle script so the logic stays in one place.
& powershell.exe -NoProfile -ExecutionPolicy Bypass -File "$toggle" -Action disable | Out-Null

Write-Output "Registered EternalMonitor VDD tasks (device resolved at trigger time; left disabled)."

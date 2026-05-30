# Removes the EternalMonitor virtual-display scheduled tasks and re-enables the device so
# it isn't left disabled if the VDD itself remains installed. Run elevated on uninstall.

$ErrorActionPreference = 'SilentlyContinue'

foreach ($task in 'EternalMonitor VDD Enable', 'EternalMonitor VDD Disable') {
    Unregister-ScheduledTask -TaskName $task -Confirm:$false
}

$dev = (Get-PnpDevice -Class Display -FriendlyName 'Virtual Display Driver' |
    Select-Object -First 1).InstanceId
if ($dev) { & pnputil.exe /enable-device "$dev" | Out-Null }

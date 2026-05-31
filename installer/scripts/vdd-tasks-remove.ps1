# Removes the EternalMonitor virtual-display scheduled tasks and DISABLES the device before the
# driver is uninstalled. Run elevated on uninstall.
#
# We disable (not enable) the device here so that if the subsequent driver uninstall step fails
# partway, the VDD can't be left behind as a phantom monitor. If the driver uninstall succeeds the
# device disappears anyway.

$ErrorActionPreference = 'SilentlyContinue'

foreach ($task in 'EternalMonitor VDD Enable', 'EternalMonitor VDD Disable') {
    Unregister-ScheduledTask -TaskName $task -Confirm:$false
}

$dev = Get-PnpDevice -Class Display |
    Where-Object { $_.FriendlyName -like '*Virtual Display*' -or $_.InstanceId -like 'ROOT\DISPLAY*' } |
    Select-Object -First 1 -ExpandProperty InstanceId
if ($dev) { & pnputil.exe /disable-device "$dev" | Out-Null }

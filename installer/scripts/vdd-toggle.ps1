# Enables or disables the bundled Virtual Display Driver device. Invoked by the
# "EternalMonitor VDD Enable" / "EternalMonitor VDD Disable" scheduled tasks (which run as
# SYSTEM with highest privileges), so the non-elevated EternalMonitor host can flip the virtual
# display on/off via `schtasks /Run` without a UAC prompt.
#
# The device is resolved AT TRIGGER TIME rather than baked in at install time, so it stays
# correct across driver versions, friendly-name changes, and PnP enumeration order. The
# VirtualDrivers/Virtual-Display-Driver enumerates under ROOT\DISPLAY and reports a friendly
# name containing "Virtual Display", so we match on either — a real monitor is never ROOT\.
param(
    [Parameter(Mandatory = $true)]
    [ValidateSet('enable', 'disable')]
    [string]$Action
)

$ErrorActionPreference = 'SilentlyContinue'

$dev = Get-PnpDevice -Class Display |
    Where-Object { $_.FriendlyName -like '*Virtual Display*' -or $_.InstanceId -like 'ROOT\DISPLAY*' } |
    Select-Object -First 1 -ExpandProperty InstanceId

if (-not $dev) {
    # Nothing to toggle (driver not installed / not yet enumerated). Not an error.
    exit 0
}

if ($Action -eq 'enable') {
    & pnputil.exe /enable-device "$dev" | Out-Null
} else {
    & pnputil.exe /disable-device "$dev" | Out-Null
}

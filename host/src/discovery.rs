use mdns_sd::{ServiceDaemon, ServiceInfo};
use tracing::{error, info};

use crate::stats::PIPELINE_STATS;

const SERVICE_TYPE: &str = "_eternaldisplay._udp.local.";
const INSTANCE_NAME: &str = "EternalMonitor";

/// Registers the EternalMonitor service on the local network via mDNS/DNS-SD
/// so the iOS app's NetworkScanner (NWBrowser) can discover it.
///
/// Returns the `ServiceDaemon` handle — dropping it unregisters the service.
pub fn advertise_service(port: u16) -> Option<ServiceDaemon> {

    let mdns = match ServiceDaemon::new() {
        Ok(d) => d,
        Err(e) => {
            error!(error = %e, "Failed to create mDNS daemon");
            return None;
        }
    };

    let host = hostname().unwrap_or_else(|| "eternal-host".to_string());
    let host_fqdn = format!("{}.local.", host);

    let properties = [("version", "0.1.1"), ("platform", "windows")];

    let instance_name = format!("{} ({})", INSTANCE_NAME, host);

    let service_info = match ServiceInfo::new(
        SERVICE_TYPE,
        &instance_name,
        &host_fqdn,
        "",
        port,
        &properties[..],
    )
    .map(|info| info.enable_addr_auto())
    {
        Ok(info) => info,
        Err(e) => {
            error!(error = %e, "Failed to create mDNS ServiceInfo");
            return None;
        }
    };

    match mdns.register(service_info) {
        Ok(_) => {
            info!(
                service_type = SERVICE_TYPE,
                port,
                host = %host,
                instance = %instance_name,
                "mDNS service registered — iPad can now discover this host"
            );
            PIPELINE_STATS.lock().mdns_active = true;
            Some(mdns)
        }
        Err(e) => {
            error!(error = %e, "Failed to register mDNS service");
            None
        }
    }
}

fn hostname() -> Option<String> {
    std::env::var("COMPUTERNAME")
        .ok()
        .or_else(|| std::env::var("HOSTNAME").ok())
}

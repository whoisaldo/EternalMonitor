use std::thread;
use std::time::Duration;

use mdns_sd::{ServiceDaemon, ServiceInfo};
use tracing::{error, info, warn};

use crate::stats::PIPELINE_STATS;

const SERVICE_TYPE: &str = "_eternaldisplay._udp.local.";
const INSTANCE_NAME: &str = "EternalMonitor";
const RE_ADVERT_INTERVAL: Duration = Duration::from_secs(60);

/// Registers the EternalMonitor service on the local network via mDNS/DNS-SD
/// so the iOS app's NetworkScanner (NWBrowser) can discover it.
///
/// Also spawns a background thread that re-registers the service every 60s.
/// Some routers and clients suppress mDNS TTL refreshes; `register` is a
/// documented upsert in mdns-sd, so the refresh never takes the service off
/// the air (the old unregister-first cycle opened a ~1s discovery hole).
///
/// Returns the `ServiceDaemon` handle — call [`say_goodbye`] on it at exit.
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
    let instance_name = format!("{} ({})", INSTANCE_NAME, host);

    let service_info = build_service_info(port, &host_fqdn, &instance_name)?;

    let full_name = service_info.get_fullname().to_string();
    let initial_addrs: Vec<_> = service_info.get_addresses().iter().copied().collect();
    info!(
        service_type = SERVICE_TYPE,
        port,
        host = %host,
        instance = %instance_name,
        addrs = ?initial_addrs,
        "mDNS service registering — advertising on all non-loopback addresses"
    );
    if let Err(e) = mdns.register(service_info) {
        error!(error = %e, "Failed to register mDNS service");
        return None;
    }
    info!("mDNS service registered — iPad can now discover this host");
    PIPELINE_STATS.lock().mdns_active = true;

    let mdns_clone = mdns.clone();
    let host_fqdn_for_thread = host_fqdn.clone();
    let instance_for_thread = instance_name.clone();
    let _ = full_name; // fullname only mattered for the old unregister cycle
    thread::spawn(move || loop {
        thread::sleep(RE_ADVERT_INTERVAL);
        let Some(info) = build_service_info(port, &host_fqdn_for_thread, &instance_for_thread)
        else {
            warn!("mDNS re-advertisement: failed to rebuild ServiceInfo");
            continue;
        };
        let addrs: Vec<_> = info.get_addresses().iter().copied().collect();
        // Plain upsert: the service stays continuously advertised (addresses
        // may have changed, e.g. DHCP renew or interface flap).
        match mdns_clone.register(info) {
            Ok(_) => info!(addrs = ?addrs, "mDNS re-advertisement"),
            Err(error) => warn!(error = %error, "mDNS re-advertisement register failed"),
        }
    });

    Some(mdns)
}

/// Send mDNS goodbye packets and stop the daemon. Called on the way out so
/// iPads drop the host from their scan list promptly instead of waiting for
/// the TTL to lapse. The short grace period lets the goodbyes hit the wire —
/// `std::process::exit` would otherwise cut them off.
pub fn say_goodbye(mdns: ServiceDaemon) {
    if mdns.shutdown().is_ok() {
        thread::sleep(Duration::from_millis(300));
        info!("mDNS goodbye sent");
    }
}

fn build_service_info(port: u16, host_fqdn: &str, instance_name: &str) -> Option<ServiceInfo> {
    let platform = if cfg!(windows) { "windows" } else { "other" };
    let properties = [
        ("version", env!("CARGO_PKG_VERSION")),
        ("proto", "2"),
        ("platform", platform),
    ];
    match ServiceInfo::new(
        SERVICE_TYPE,
        instance_name,
        host_fqdn,
        "",
        port,
        &properties[..],
    )
    .map(|info| info.enable_addr_auto())
    {
        Ok(info) => Some(info),
        Err(e) => {
            error!(error = %e, "Failed to create mDNS ServiceInfo");
            None
        }
    }
}

fn hostname() -> Option<String> {
    std::env::var("COMPUTERNAME")
        .ok()
        .or_else(|| std::env::var("HOSTNAME").ok())
}

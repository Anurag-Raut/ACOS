//! Measure the cost of hosting the launch set.
//!
//! ```text
//! cargo run -q -p acos-services --example measure-host
//! ```
//!
//! `/usr/bin/time` is not assumed; peak RSS is read from `/proc/self/status`
//! (`VmHWM`), as the service-layer REPORT does.

use std::time::Instant;

use acos_services::value::Value;
use acos_services::ServiceHost;

fn status_field(name: &str) -> Option<u64> {
    let text = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix(name) {
            let rest = rest.trim_start_matches(':').trim();
            return rest
                .trim_end_matches(" kB")
                .trim()
                .parse::<u64>()
                .ok();
        }
    }
    None
}

fn main() {
    let vmrss_before = status_field("VmRSS").unwrap_or(0);
    let vmsize_before = status_field("VmSize").unwrap_or(0);

    let t0 = Instant::now();
    let mut host = ServiceHost::standard();
    let install_ns = t0.elapsed().as_nanos() as u64;

    let t1 = Instant::now();
    // The launch set: the nine `IServiceManager.checkService` lookups the
    // framework makes, which lazily construct exactly those services.
    let launch_names = [
        "activity",
        "package",
        "display",
        "accessibility",
        "input",
        "graphicsstats",
        "content",
        "game",
        "audio",
    ];
    let mut binders = Vec::new();
    for name in launch_names {
        binders.push(host.check_service(name).unwrap().unwrap());
    }
    let construct_ns = t1.elapsed().as_nanos() as u64;

    let t2 = Instant::now();
    let activity = binders[0];
    for _ in 0..100 {
        let _ = host.invoke(
            activity,
            "android.app.IActivityManager",
            "checkPermission",
            &[
                Value::Str(Some("android.permission.INTERNET".into())),
                Value::Int(10192),
                Value::Int(1),
            ],
        );
    }
    let call_100_ns = t2.elapsed().as_nanos() as u64;

    let vmrss_after = status_field("VmRSS").unwrap_or(0);
    let vmhwm_after = status_field("VmHWM").unwrap_or(0);
    let vmsize_after = status_field("VmSize").unwrap_or(0);

    println!("ServiceHost::standard() install: {install_ns} ns");
    println!("lazily construct {} services:   {construct_ns} ns", launch_names.len());
    println!("100 replayed checkPermission calls: {call_100_ns} ns ({} ns/call)", call_100_ns / 100);
    println!("VmRSS:  {vmrss_before} kB -> {vmrss_after} kB");
    println!("VmSize: {vmsize_before} kB -> {vmsize_after} kB");
    println!("VmHWM:  {vmhwm_after} kB");
    println!("constructions: {}", host.constructions());
    println!("registered descriptors: {}", host.descriptors().len());
    println!("registered service names: {}", host.service_names().len());

    let _ = host.take_deliveries();
    let _ = host.drain_events();
}

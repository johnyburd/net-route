//! Route operation integration tests for macOS.

#![cfg(target_os = "macos")]

use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use net_route::{Handle, Route};

// One distinct destination per test so they can run in parallel without colliding.
const GW_NET: (IpAddr, u8) = (IpAddr::V4(Ipv4Addr::new(198, 51, 100, 0)), 24);
const IFACE_NET: (IpAddr, u8) = (IpAddr::V4(Ipv4Addr::new(198, 51, 100, 128)), 25);
const DEL_NET: (IpAddr, u8) = (IpAddr::V4(Ipv4Addr::new(198, 51, 100, 64)), 26);

fn is_unspecified(addr: IpAddr) -> bool {
    match addr {
        IpAddr::V4(a) => a == Ipv4Addr::UNSPECIFIED,
        IpAddr::V6(a) => a == Ipv6Addr::UNSPECIFIED,
    }
}

async fn find(handle: &Handle, destination: IpAddr, prefix: u8) -> io::Result<Option<Route>> {
    Ok(handle
        .list()
        .await?
        .into_iter()
        .find(|r| r.destination == destination && r.prefix == prefix))
}

/// Index of the loopback interface.
fn loopback_ifindex() -> io::Result<u32> {
    net_route::ifname_to_index("lo0").ok_or_else(|| io::Error::other("lo0 not found"))
}

/// IPv4 next hop of the host's default route, if it has one.
async fn v4_gateway(handle: &Handle) -> io::Result<Option<IpAddr>> {
    Ok(handle
        .list()
        .await?
        .into_iter()
        .find(|r| r.destination == IpAddr::V4(Ipv4Addr::UNSPECIFIED) && r.prefix == 0)
        .and_then(|r| r.gateway)
        .filter(|gw| matches!(gw, IpAddr::V4(_))))
}

/// Add a gateway route, get it back from the routing table, then remove it.
#[tokio::test]
async fn add_get_gateway_route() -> io::Result<()> {
    let handle = Handle::new()?;
    let Some(gateway) = v4_gateway(&handle).await? else {
        eprintln!("skipping: host has no IPv4 default gateway");
        return Ok(());
    };

    let route = Route::new(GW_NET.0, GW_NET.1).with_gateway(gateway);
    handle.add(&route).await?;
    let added = find(&handle, GW_NET.0, GW_NET.1).await?;
    handle.delete(&route).await?;

    let added = added.expect("route added via gateway should appear in the routing table");
    assert_eq!(added.prefix, GW_NET.1);
    assert_eq!(added.gateway, Some(gateway));
    Ok(())
}

/// Add a route through an interface only and get it back.
#[tokio::test]
async fn add_get_interface_route() -> io::Result<()> {
    let handle = Handle::new()?;
    let lo = loopback_ifindex()?;

    let route = Route::new(IFACE_NET.0, IFACE_NET.1).with_ifindex(lo);
    handle.add(&route).await?;
    let added = find(&handle, IFACE_NET.0, IFACE_NET.1).await?;
    handle.delete(&route).await?;

    let added = added.expect("interface route should appear in the routing table");
    assert_eq!(added.prefix, IFACE_NET.1);
    // The kernel keeps the interface in the gateway slot as a link-layer address,
    // which `list` reports as no IP gateway.
    assert_eq!(added.gateway, None);
    Ok(())
}

/// A deleted route is gone from the routing table.
#[tokio::test]
async fn delete_route() -> io::Result<()> {
    let handle = Handle::new()?;
    let lo = loopback_ifindex()?;

    let route = Route::new(DEL_NET.0, DEL_NET.1).with_ifindex(lo);
    handle.add(&route).await?;
    assert!(
        find(&handle, DEL_NET.0, DEL_NET.1).await?.is_some(),
        "route should be present after add"
    );

    handle.delete(&route).await?;
    assert!(
        find(&handle, DEL_NET.0, DEL_NET.1).await?.is_none(),
        "route should be gone after delete"
    );
    Ok(())
}

/// `list` returns the routing table, including whatever `default_route` reports.
#[tokio::test]
async fn list_routes() -> io::Result<()> {
    let handle = Handle::new()?;
    let routes = handle.list().await?;

    assert!(!routes.is_empty(), "routing table should not be empty");
    if let Some(default) = handle.default_route().await? {
        assert!(
            routes.iter().any(|r| {
                r.destination == default.destination
                    && r.prefix == default.prefix
                    && r.gateway == default.gateway
            }),
            "default route {default:?} missing from list()"
        );
    }
    Ok(())
}

/// `default_route` returns a /0 route for the unspecified address, or None if the
/// host has no default route at all.
#[tokio::test]
async fn default_route() -> io::Result<()> {
    let handle = Handle::new()?;
    if let Some(route) = handle.default_route().await? {
        assert_eq!(route.prefix, 0);
        assert!(is_unspecified(route.destination));
    }
    Ok(())
}

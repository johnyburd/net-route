//! Route operation integration tests for Windows.

#![cfg(target_os = "windows")]

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

/// Index of the loopback interface, taken from its 127.0.0.0/8 route.
async fn loopback_ifindex(handle: &Handle) -> io::Result<u32> {
    handle
        .list()
        .await?
        .into_iter()
        .find(|r| r.destination == IpAddr::V4(Ipv4Addr::LOCALHOST) && r.prefix == 8)
        .and_then(|r| r.ifindex)
        .ok_or_else(|| io::Error::other("no 127.0.0.0/8 route to derive the loopback index from"))
}

/// The host's IPv4 default route, with a real IPv4 next hop and interface.
/// `CreateIpForwardEntry2` requires an interface, so gateway test routes carry both.
async fn v4_default_route(handle: &Handle) -> io::Result<Option<Route>> {
    Ok(handle.list().await?.into_iter().find(|r| {
        r.destination == IpAddr::V4(Ipv4Addr::UNSPECIFIED)
            && r.prefix == 0
            && matches!(r.gateway, Some(IpAddr::V4(_)))
            && r.ifindex.is_some()
    }))
}

/// Add a gateway route, get it back from the routing table, then remove it.
#[tokio::test]
async fn add_get_gateway_route() -> io::Result<()> {
    let handle = Handle::new()?;
    let Some(default) = v4_default_route(&handle).await? else {
        eprintln!("skipping: host has no IPv4 default route with a gateway");
        return Ok(());
    };
    let gateway = default.gateway.unwrap();

    let route = Route::new(GW_NET.0, GW_NET.1)
        .with_gateway(gateway)
        .with_ifindex(default.ifindex.unwrap());
    handle.add(&route).await?;
    let added = find(&handle, GW_NET.0, GW_NET.1).await?;
    handle.delete(&route).await?;

    let added = added.expect("route added via gateway should appear in the routing table");
    assert_eq!(added.prefix, GW_NET.1);
    assert_eq!(added.gateway, Some(gateway));
    Ok(())
}

/// Add a route through an interface only (no gateway), read it back, then remove it.
#[tokio::test]
async fn add_get_interface_route() -> io::Result<()> {
    let handle = Handle::new()?;
    let lo = loopback_ifindex(&handle).await?;

    let route = Route::new(IFACE_NET.0, IFACE_NET.1).with_ifindex(lo);
    handle.add(&route).await?;
    let added = find(&handle, IFACE_NET.0, IFACE_NET.1).await?;
    handle.delete(&route).await?;

    let added = added.expect("interface route should appear in the routing table");
    assert_eq!(added.prefix, IFACE_NET.1);
    // Windows stores the interface route with an unspecified next hop.
    assert!(
        added.gateway.is_none() || is_unspecified(added.gateway.unwrap()),
        "interface route should have no real gateway, got {:?}",
        added.gateway
    );
    Ok(())
}

/// A deleted route is gone from the routing table.
#[tokio::test]
async fn delete_route() -> io::Result<()> {
    let handle = Handle::new()?;
    let lo = loopback_ifindex(&handle).await?;

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

/// `list` returns the routing table, including the loopback network.
#[tokio::test]
async fn list_routes() -> io::Result<()> {
    let handle = Handle::new()?;
    let routes = handle.list().await?;

    assert!(!routes.is_empty(), "routing table should not be empty");
    assert!(
        routes
            .iter()
            .any(|r| r.destination == IpAddr::V4(Ipv4Addr::LOCALHOST) && r.prefix == 8),
        "loopback route 127.0.0.0/8 missing from the routing table"
    );
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

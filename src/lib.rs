// SPDX-License-Identifier: MIT

//! This project aims to provide a high level interface for manipulating and observing
//! the routing table on a variety of platforms.
//!
//!
//! ## Examples
//! #### Adding a route
//! ```
//! // route traffic destined for 10.14.0.0/24 to 192.1.2.1 using interface 9
//! let handle = Handle::new()?;
//! let route = Route::new("10.14.0.0".parse().unwrap(), 24)
//!     .with_ifindex(9)
//!     .with_gateway("192.1.2.1".parse.unwrap());
//! handle.add(&route).await
//! ```
//!
//! #### Listening to changes in the routing table
//! ```
//! let handle = Handle::new()?;
//! let stream = handle.route_listen_stream();
//! futures::pin_mut!(stream);
//! while let Some(event) = stream.next().await {
//!     println!("{:?}", value);
//! }
//! ```

use std::{
    io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
};

mod platform_impl;
use platform_impl::PlatformHandle;

#[cfg(all(target_os = "macos", not(doc)))]
pub use platform_impl::ifname_to_index;

/// Handle that abstracts initialization and cleanup of resources needed to operate on the routing table.
pub struct Handle(PlatformHandle);

impl Handle {
    pub fn new() -> io::Result<Self> {
        Ok(Self(PlatformHandle::new()?))
    }

    /// Add route to the system's routing table.
    pub async fn add(&self, route: &Route) -> io::Result<()> {
        self.0.add(route).await
    }

    /// Returns a `Stream` which will yield a `RouteChange` event whenever a route is added, removed, or changed from the system's routing table.
    pub fn route_listen_stream(&self) -> impl futures::Stream<Item = RouteChange> {
        self.0.route_listen_stream()
    }

    /// Returns a `Vec<Route>` containing a list of both ipv4 and v6 routes on the system.
    pub async fn list(&self) -> io::Result<Vec<Route>> {
        self.0.list().await
    }

    /// Get one of the default routes on the system if there is at least one.
    pub async fn default_route(&self) -> io::Result<Option<Route>> {
        self.0.default_route().await
    }

    /// Remove a route from the system's routing table.
    pub async fn delete(&self, route: &Route) -> io::Result<()> {
        self.0.delete(route).await
    }
}

/// Contains information that describes a route in the local computer's Ipv4 or Ipv6 routing table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    /// Network address of the destination. `0.0.0.0` with a prefix of `0` is considered a default route.
    pub destination: IpAddr,

    /// Length of network prefix in the destination address.
    pub prefix: u8,

    /// The address of the next hop of this route.
    ///
    /// On macOS, this must be `Some` if ifindex is `None`
    pub gateway: Option<IpAddr>,

    /// The index of the local interface through which the next hop of this route may be reached.
    ///
    /// On macOS, this must be `Some` if gateway is `None`
    pub ifindex: Option<u32>,

    #[cfg(target_os = "linux")]
    /// The routing table this route belongs to.
    pub table: u8,

    #[cfg(target_os = "windows")]
    /// The route metric offset value for this route.
    pub metric: Option<u32>,

    #[cfg(target_os = "windows")]
    /// Luid of the local interface through which the next hop of this route may be reached.
    ///
    /// If luid is specified, ifindex is optional.
    pub luid: Option<u64>,
}

impl Route {
    /// Create a route that matches a given destination network.
    ///
    /// Either the gateway or interface should be set before attempting to add to a routing table.
    pub fn new(destination: IpAddr, prefix: u8) -> Self {
        Self {
            destination,
            prefix,
            gateway: None,
            ifindex: None,
            #[cfg(target_os = "linux")]
            // default to main table
            table: 254,
            #[cfg(target_os = "windows")]
            metric: None,
            #[cfg(target_os = "windows")]
            luid: None,
        }
    }

    /// Set the next next hop gateway for this route.
    pub fn with_gateway(mut self, gateway: IpAddr) -> Self {
        self.gateway = Some(gateway);
        self
    }

    /// Set the index of the local interface through which the next hop of this route should be reached.
    pub fn with_ifindex(mut self, ifindex: u32) -> Self {
        self.ifindex = Some(ifindex);
        self
    }

    /// Set table the route will be installed in.
    #[cfg(target_os = "linux")]
    pub fn with_table(mut self, table: u8) -> Self {
        self.table = table;
        self
    }

    /// Set route metric.
    #[cfg(target_os = "windows")]
    pub fn with_metric(mut self, metric: u32) -> Self {
        self.metric = Some(metric);
        self
    }

    /// Set luid of the local interface through which the next hop of this route should be reached.
    #[cfg(target_os = "windows")]
    pub fn with_luid(mut self, luid: u64) -> Self {
        self.luid = Some(luid);
        self
    }

    /// Get the netmask covering the network portion of the destination address.
    pub fn mask(&self) -> IpAddr {
        match self.destination {
            IpAddr::V4(_) => IpAddr::V4(Ipv4Addr::from(
                u32::MAX.checked_shl(32 - self.prefix as u32).unwrap_or(0),
            )),
            IpAddr::V6(_) => IpAddr::V6(Ipv6Addr::from(
                u128::MAX.checked_shl(128 - self.prefix as u32).unwrap_or(0),
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteChange {
    Add(Route),
    Delete(Route),
    Change(Route),
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv6Addr};

    use crate::Route;

    #[test]
    fn it_calculates_v4_netmask() {
        let mut route = Route::new("10.10.0.0".parse().unwrap(), 32);

        assert_eq!(route.mask(), "255.255.255.255".parse::<IpAddr>().unwrap());

        route.prefix = 29;
        assert_eq!(route.mask(), "255.255.255.248".parse::<IpAddr>().unwrap());

        route.prefix = 25;
        assert_eq!(route.mask(), "255.255.255.128".parse::<IpAddr>().unwrap());

        route.prefix = 2;
        assert_eq!(route.mask(), "192.0.0.0".parse::<IpAddr>().unwrap());
    }

    #[test]
    fn it_calculates_v6_netmask() {
        let route = Route::new(
            "77ca:838b:9ec0:fc97:eedc:236a:9d41:31e5".parse().unwrap(),
            32,
        );
        assert_eq!(
            route.mask(),
            Ipv6Addr::new(0xffff, 0xffff, 0, 0, 0, 0, 0, 0)
        );
    }
}

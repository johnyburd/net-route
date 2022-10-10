use std::{io, net::{IpAddr, Ipv4Addr, Ipv6Addr}};

mod platform_impl;
use platform_impl::PlatformHandle;


/// Handle that abstracts initialization and cleanup of resources needed to operate on the routing table.
pub struct Handle(PlatformHandle);

impl Handle {
    pub fn new() -> io::Result<Self> {
        Ok(Self(PlatformHandle::new()?))
    }

    /// Fetch next-hop gateway for one of the default routes on the system
    #[deprecated(since="0.1.3", note="I'm still figuring stuff out")]
    pub async fn default_gateway(&self) -> io::Result<IpAddr> {
        self.0.default_gateway().await
    }

    /// Add route to the system's routing table.
    pub async fn add(&self, route: &Route) -> io::Result<()> {
        if route.gateway.is_some() && route.ifindex.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "`gateway` and `ifindex` cannot both be set.",
            ));
        } else if route.gateway.is_none() && route.ifindex.is_none() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "`gateway` and `ifindex` cannot both be none.",
            ));
        }
        self.0.add(route).await
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
    /// This must be `Some` if ifindex is `None`
    pub gateway: Option<IpAddr>,

    /// The index of the local interface through which the next hop of this route should be reached.
    /// 
    /// This must be `Some` if gateway is `None`
    pub ifindex: Option<u32>,

   #[cfg(target_os = "linux")]
    /// 
    pub table: u8,
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

    pub fn mask(&self) -> IpAddr {
        match self.destination {
            IpAddr::V4(_) => IpAddr::V4(Ipv4Addr::from(u32::MAX << (32 - self.prefix))),
            IpAddr::V6(_) => IpAddr::V6(Ipv6Addr::from(u128::MAX << (128 - self.prefix))),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv6Addr};

    use crate::{Handle, Route};

    #[tokio::test]
    async fn it_gets_default_gateway() {
        let handle = Handle::new().unwrap();
        #[allow(deprecated)]
        let router = handle.default_gateway().await.unwrap();
        
        println!("default router: {}", router);
    }

    #[test]
    fn it_calculates_v4_netmask() {
        let mut route = Route {
            destination: "10.10.0.0".parse().unwrap(),
            prefix: 32,
            gateway: None,
            ifindex: None,
        };

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
        let route = Route {
            destination: "77ca:838b:9ec0:fc97:eedc:236a:9d41:31e5".parse().unwrap(),
            prefix: 32,
            gateway: None,
            ifindex: None,
        };
        assert_eq!(route.mask(), Ipv6Addr::new(0xffff, 0xffff, 0, 0, 0, 0, 0, 0));
    }

    #[tokio::test]
    async fn it_adds_routes() {
        let handle = Handle::new().unwrap();
        let route = Route::new("10.10.0.1".parse().unwrap(), 32)
            .with_gateway("192.168.1.1".parse().unwrap());
        println!("route {:?}", route);
        handle.add(&route).await.unwrap();
    }
}

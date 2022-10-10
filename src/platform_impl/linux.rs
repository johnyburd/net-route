use std::io::{Error, self};
use crate::Route;

use std::net::IpAddr;
use futures::stream::TryStreamExt;
use tokio::task::JoinHandle;

pub struct Handle {
    handle: rtnetlink::Handle,
    join_handle: JoinHandle<()>,
}

impl Handle {
    pub(crate) fn new() -> io::Result<Self> {
        let (connection, handle, _) = rtnetlink::new_connection()?;
        let join_handle = tokio::spawn(connection);

        Ok(Self {
            handle,
            join_handle,
        })
    }

    pub(crate) async fn default_gateway(&self) -> io::Result<IpAddr> {

        let mut routes = self.handle.route().get(rtnetlink::IpVersion::V4).execute();

        while let Some(route) = routes.try_next().await.unwrap() {
            if route.destination_prefix().is_none() {
                return Ok(route.gateway().unwrap())
            }
        }
        Err(Error::new(io::ErrorKind::Other, "No ipv4 default route"))
    }

    pub(crate) async fn add(&self, route: &Route) -> io::Result<()> {
        let route_handle = self.handle.route();
        match route.destination {
            IpAddr::V4(addr) => {
                let mut msg = route_handle.add().v4().destination_prefix(addr, route.prefix);

                if let Some(ifindex) = route.ifindex {
                    msg = msg.output_interface(ifindex);
                }

                if let Some(gateway) = route.gateway {
                    msg = match gateway {
                        IpAddr::V4(addr) => msg.gateway(addr),
                        IpAddr::V6(_) => return Err(Error::new(
                            io::ErrorKind::InvalidInput,
                            "gateway version must match destination",
                        )),
                    };
                }
                msg.execute().await.map_err(|e| Error::new(io::ErrorKind::Other, e.to_string()))
            }
            IpAddr::V6(addr) => {
                let mut msg = route_handle.add().v6().destination_prefix(addr, route.prefix);

                if let Some(ifindex) = route.ifindex {
                    msg = msg.output_interface(ifindex);
                }

                if let Some(gateway) = route.gateway {
                    msg = match gateway {
                        IpAddr::V6(addr) => msg.gateway(addr),
                        IpAddr::V4(_) => return Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "gateway version must match destination",
                        )),
                    };
                }
                msg.execute().await.map_err(|e| Error::new(io::ErrorKind::Other, e.to_string()))
            }
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.join_handle.abort();
    }
}
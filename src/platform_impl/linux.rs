use crate::{Route, RouteChange};
use std::io::{self, Error};

use async_stream::stream;
use futures::{channel::mpsc::UnboundedReceiver, stream::TryStreamExt};
use futures::{Stream, StreamExt};
use netlink_packet_core::{NetlinkMessage, NetlinkPayload};
use netlink_packet_route::{route, RouteMessage, RtnlMessage};
use netlink_sys::{AsyncSocket, SocketAddr};
use std::net::IpAddr;
use tokio::{sync::broadcast, task::JoinHandle};

use rtnetlink::{
    constants::{RTMGRP_IPV4_ROUTE, RTMGRP_IPV6_ROUTE},
    new_connection,
};

pub struct Handle {
    handle: rtnetlink::Handle,
    join_handle: JoinHandle<()>,
    listen_handle: JoinHandle<()>,
    tx: broadcast::Sender<RouteChange>,
}

impl Handle {
    pub(crate) fn new() -> io::Result<Self> {
        let (mut connection, handle, messages) = new_connection()?;

        // These flags specify what kinds of broadcast messages we want to listen for.
        let mgroup_flags = RTMGRP_IPV4_ROUTE | RTMGRP_IPV6_ROUTE;

        // A netlink socket address is created with said flags.
        let addr = SocketAddr::new(0, mgroup_flags);
        // Said address is bound so new conenctions and thus new message broadcasts can be received.
        connection.socket_mut().socket_mut().bind(&addr)?;
        let (tx, _) = broadcast::channel::<RouteChange>(16);

        let join_handle = tokio::spawn(connection);
        let listen_handle = tokio::spawn(Self::listen(messages, tx.clone()));

        Ok(Self {
            handle,
            join_handle,
            listen_handle,
            tx,
        })
    }

    pub(crate) async fn default_route(&self) -> io::Result<Option<Route>> {
        let mut routes = self.handle.route().get(rtnetlink::IpVersion::V4).execute();

        while let Some(route) = routes
            .try_next()
            .await
            .map_err(|e| Error::new(io::ErrorKind::Other, e.to_string()))?
        {
            if route.destination_prefix().is_none() {
                return Ok(Some(route.into()));
            }
        }

        let mut routes = self.handle.route().get(rtnetlink::IpVersion::V6).execute();

        while let Some(route) = routes
            .try_next()
            .await
            .map_err(|e| Error::new(io::ErrorKind::Other, e.to_string()))?
        {
            if route.destination_prefix().is_none() {
                return Ok(Some(route.into()));
            }
        }
        Ok(None)
    }

    pub(crate) async fn list(&self) -> io::Result<Vec<Route>> {
        let mut routes = vec![];
        let mut route_messages = self.handle.route().get(rtnetlink::IpVersion::V4).execute();

        while let Some(route) = route_messages
            .try_next()
            .await
            .map_err(|e| Error::new(io::ErrorKind::Other, e.to_string()))?
        {
            routes.push(route.into());
        }

        let mut route_messages = self.handle.route().get(rtnetlink::IpVersion::V6).execute();

        while let Some(route) = route_messages
            .try_next()
            .await
            .map_err(|e| Error::new(io::ErrorKind::Other, e.to_string()))?
        {
            routes.push(route.into());
        }
        Ok(routes)
    }

    pub(crate) fn route_listen_stream(&self) -> impl Stream<Item = RouteChange> {
        let mut rx = self.tx.subscribe();
        stream! {
            loop {
                match rx.recv().await {
                    Ok(ev) => yield ev,
                    Err(e) => match e {
                        broadcast::error::RecvError::Closed => break,
                        broadcast::error::RecvError::Lagged(_) => continue,
                    }
                }
            }
        }
    }

    pub(crate) async fn delete(&self, route: &Route) -> io::Result<()> {
        let route_handle = self.handle.route();
        let mut routes = match route.destination {
            IpAddr::V4(_) => route_handle.get(rtnetlink::IpVersion::V4),
            IpAddr::V6(_) => route_handle.get(rtnetlink::IpVersion::V6),
        }
        .execute();

        while let Some(msg) = routes
            .try_next()
            .await
            .map_err(|e| Error::new(io::ErrorKind::Other, e.to_string()))?
        {
            let other_route: Route = msg.clone().into();
            if other_route.destination == route.destination && other_route.prefix == route.prefix {
                route_handle
                    .del(msg)
                    .execute()
                    .await
                    .map_err(|e| Error::new(io::ErrorKind::Other, e.to_string()))?;
                return Ok(());
            }
        }

        Err(Error::new(
            io::ErrorKind::NotFound,
            "No matching route found to delete",
        ))
    }

    pub(crate) async fn add(&self, route: &Route) -> io::Result<()> {
        let route_handle = self.handle.route();
        match route.destination {
            IpAddr::V4(addr) => {
                let mut msg = route_handle
                    .add()
                    .v4()
                    .table_id(route.table.into())
                    .destination_prefix(addr, route.prefix);

                if let Some(ifindex) = route.ifindex {
                    msg = msg.output_interface(ifindex);
                }

                if let Some(gateway) = route.gateway {
                    msg = match gateway {
                        IpAddr::V4(addr) => msg.gateway(addr),
                        IpAddr::V6(_) => {
                            return Err(Error::new(
                                io::ErrorKind::InvalidInput,
                                "gateway version must match destination",
                            ))
                        }
                    };
                }
                msg.execute()
                    .await
                    .map_err(|e| Error::new(io::ErrorKind::Other, e.to_string()))
            }
            IpAddr::V6(addr) => {
                let mut msg = route_handle
                    .add()
                    .v6()
                    .table_id(route.table.into())
                    .destination_prefix(addr, route.prefix);

                if let Some(ifindex) = route.ifindex {
                    msg = msg.output_interface(ifindex);
                }

                if let Some(gateway) = route.gateway {
                    msg = match gateway {
                        IpAddr::V6(addr) => msg.gateway(addr),
                        IpAddr::V4(_) => {
                            return Err(io::Error::new(
                                io::ErrorKind::InvalidInput,
                                "gateway version must match destination",
                            ))
                        }
                    };
                }
                msg.execute()
                    .await
                    .map_err(|e| Error::new(io::ErrorKind::Other, e.to_string()))
            }
        }
    }

    async fn listen(
        mut messages: UnboundedReceiver<(NetlinkMessage<RtnlMessage>, SocketAddr)>,
        tx: broadcast::Sender<RouteChange>,
    ) {
        while let Some((message, _)) = messages.next().await {
            if let NetlinkPayload::InnerMessage(msg) = message.payload {
                match msg {
                    RtnlMessage::NewRoute(msg) => _ = tx.send(RouteChange::Add(msg.into())),
                    RtnlMessage::DelRoute(msg) => _ = tx.send(RouteChange::Delete(msg.into())),
                    _ => (),
                }
            }
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.join_handle.abort();
        self.listen_handle.abort();
    }
}

fn vec_to_ip(addr: Vec<u8>) -> Option<IpAddr> {
    match addr.len() {
        4 => {
            let addr: [u8; 4] = addr.try_into().unwrap();
            Some(IpAddr::from(addr))
        }
        16 => {
            let addr: [u8; 16] = addr.try_into().unwrap();
            Some(IpAddr::from(addr))
        }
        _ => None,
    }
}

impl From<RouteMessage> for Route {
    fn from(msg: RouteMessage) -> Self {
        let mut gateway = None;
        let mut destination = None;
        let mut ifindex = None;

        for nla in msg.nlas {
            match nla {
                route::Nla::Destination(addr) => {
                    destination = vec_to_ip(addr);
                }
                route::Nla::Gateway(addr) => {
                    gateway = vec_to_ip(addr);
                }
                route::Nla::Oif(i) => {
                    ifindex = Some(i);
                }
                _ => {}
            }
        }
        // rtnetlink gives None instead of 0.0.0.0 for the default route, but we'll convert to 0 here to make it match the other platforms
        let destination = destination.unwrap_or_else(|| match msg.header.address_family {
            2 => IpAddr::from([0u8; 4]),
            10 => IpAddr::from([0u8; 16]),
            _ => panic!("invalid destination family"),
        });
        Self {
            destination,
            prefix: msg.header.destination_prefix_length,
            gateway,
            ifindex,
            table: msg.header.table,
        }
    }
}

use async_stream::stream;
use futures::Stream;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::{io, net::IpAddr};
use tokio::sync::broadcast;
use winapi::ctypes::c_void;
use winapi::shared::netioapi::{
    CancelMibChangeNotify2, CreateIpForwardEntry2, FreeMibTable, GetIpForwardTable2,
    MibAddInstance, MibDeleteInstance, MibParameterNotification, NotifyRouteChange2,
    MIB_NOTIFICATION_TYPE, PMIB_IPFORWARD_TABLE2, DeleteIpForwardEntry2,
};
use winapi::shared::netioapi::{InitializeIpForwardEntry, MIB_IPFORWARD_ROW2};
use winapi::shared::ntdef::HANDLE;
use winapi::shared::winerror::ERROR_SUCCESS;
use winapi::shared::ws2def::{AF_INET, AF_INET6, AF_UNSPEC};
use winapi::shared::wtypesbase::BOOLEAN;

use crate::{Route, RouteChange};

unsafe fn row_to_route(row: *mut MIB_IPFORWARD_ROW2) -> Option<Route> {
    let dst_family = (*row).DestinationPrefix.Prefix.si_family();
    let dst = match *dst_family as i32 {
        AF_INET => IpAddr::from(std::mem::transmute::<_, [u8; 4]>(
            (*row).DestinationPrefix.Prefix.Ipv4().sin_addr,
        )),
        AF_INET6 => IpAddr::from(std::mem::transmute::<_, [u8; 16]>(
            (*row).DestinationPrefix.Prefix.Ipv6().sin6_addr,
        )),
        _ => panic!("Unexpected family {}", dst_family),
    };

    let dst_len = (*row).DestinationPrefix.PrefixLength;

    let nexthop_family = (*row).NextHop.si_family();

    let gateway = match *nexthop_family as i32 {
        AF_INET => Some(IpAddr::from(std::mem::transmute::<_, [u8; 4]>(
            (*row).NextHop.Ipv4().sin_addr,
        ))),
        AF_INET6 => Some(IpAddr::from(std::mem::transmute::<_, [u8; 16]>(
            (*row).NextHop.Ipv6().sin6_addr,
        ))),
        _ => None,
    };

    let mut route = Route::new(dst, dst_len)
        .with_ifindex((*row).InterfaceIndex)
        .with_luid(std::mem::transmute((*row).InterfaceLuid))
        .with_metric((*row).Metric);

    route.gateway = gateway;
    Some(route)
}

unsafe extern "system" fn callback(
    callercontext: *mut c_void,
    row: *mut MIB_IPFORWARD_ROW2,
    notification_type: MIB_NOTIFICATION_TYPE,
) {
    let tx = &*(callercontext as *const broadcast::Sender<RouteChange>);

    if let Some(route) = row_to_route(row) {
        let event = match notification_type {
            n if n == MibParameterNotification => RouteChange::Change(route),
            n if n == MibAddInstance => RouteChange::Add(route),
            n if n == MibDeleteInstance => RouteChange::Delete(route),
            _ => return,
        };
        _ = tx.send(event)
    }
}

fn code_to_error(code: u32, msg: &str) -> io::Error {
    let kind = match code {
        2 => io::ErrorKind::NotFound,
        5 => io::ErrorKind::PermissionDenied,
        87 => io::ErrorKind::InvalidInput,
        5010 => io::ErrorKind::AlreadyExists,
        1168 => io::ErrorKind::NotFound,
        _ => io::ErrorKind::Other,
    };
    io::Error::new(kind, format!("{}: {}", code, msg))
}

pub(crate) struct Handle {
    handle: HANDLE,
    tx: broadcast::Sender<RouteChange>,
    _tx: Box<broadcast::Sender<RouteChange>>,
}

impl Handle {
    pub fn new() -> io::Result<Self> {
        let mut handle: HANDLE = std::ptr::null_mut();

        let (tx, _) = broadcast::channel::<RouteChange>(16);
        let mut tx_clone = Box::new(tx.clone());

        // TODO we could wait until `route_listen_stream` is called to initialize this
        let ret = unsafe {
            NotifyRouteChange2(
                AF_UNSPEC as u16,
                Some(callback),
                (tx_clone.as_mut() as *mut _) as *mut _,
                BOOLEAN::from(false),
                &mut handle,
            )
        };
        if ret != ERROR_SUCCESS {
            return Err(code_to_error(ret, "Error creating listener: {}"));
        }
        Ok(Self {
            handle,
            tx,
            _tx: tx_clone,
        })
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
        let row: MIB_IPFORWARD_ROW2 = route.into();

        let err = unsafe { DeleteIpForwardEntry2(&row) };
        if err != ERROR_SUCCESS {
            return Err(code_to_error(err, "error deleting entry"));
        }
        Ok(())
    }

    pub(crate) async fn default_route(&self) -> io::Result<Option<Route>> {
        for route in self.list().await? {
            if (route.destination == Ipv4Addr::UNSPECIFIED
                || route.destination == Ipv6Addr::UNSPECIFIED)
                && route.prefix == 0
                && route.gateway != Some(IpAddr::V4(Ipv4Addr::UNSPECIFIED))
                && route.gateway != Some(IpAddr::V6(Ipv6Addr::UNSPECIFIED))
            {
                return Ok(Some(route));
            }
        }
        Ok(None)
    }

    pub(crate) async fn list(&self) -> io::Result<Vec<Route>> {
        let mut ptable: PMIB_IPFORWARD_TABLE2 = std::ptr::null_mut();

        let ret = unsafe { GetIpForwardTable2(AF_UNSPEC as u16, &mut ptable) };
        if ret != ERROR_SUCCESS {
            return Err(code_to_error(ret, "Error getting table: {}"));
        }

        let prows = unsafe {
            std::ptr::slice_from_raw_parts(
                &(*ptable).Table as *const _ as *const MIB_IPFORWARD_ROW2,
                (*ptable).NumEntries as usize,
            )
        };

        let entries = unsafe { (*ptable).NumEntries };
        let res = (0..entries)
            .map(|idx| unsafe { (*prows)[idx as usize] })
            .filter_map(|mut row| unsafe { row_to_route(&mut row) })
            .collect::<Vec<_>>();
        unsafe { FreeMibTable(ptable as *mut _ as *mut _) };
        Ok(res)
    }

    pub(crate) async fn add(&self, route: &Route) -> io::Result<()> {
        let row: MIB_IPFORWARD_ROW2 = route.into();

        let err = unsafe { CreateIpForwardEntry2(&row) };
        if err != ERROR_SUCCESS {
            return Err(code_to_error(err, "error creating entry"));
        }
        Ok(())
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            CancelMibChangeNotify2(self.handle);
        }
    }
}

unsafe impl Send for Handle {}
unsafe impl Sync for Handle {}

impl From<&Route> for MIB_IPFORWARD_ROW2 {
    fn from(route: &Route) -> Self {
        let mut row: MIB_IPFORWARD_ROW2 = unsafe { std::mem::zeroed() };
        unsafe { InitializeIpForwardEntry(&mut row) };

        if let Some(ifindex) = route.ifindex {
            /*let mut luid: NET_LUID = unsafe { std::mem::zeroed() };
            let err = unsafe { ConvertInterfaceIndexToLuid(ifindex, &mut luid) };
            if err != ERROR_SUCCESS {
                return Err(io::Error::new(io::ErrorKind::Other, format!("Error from luid {}", err)));
            }*/
            row.InterfaceIndex = ifindex;
        }

        if let Some(luid) = route.luid {
            row.InterfaceLuid = unsafe { std::mem::transmute(luid) };
        }

        if let Some(gateway) = route.gateway {
            match gateway {
                IpAddr::V4(addr) => unsafe {
                    *row.NextHop.si_family_mut() = AF_INET as u16;
                    row.NextHop.Ipv4_mut().sin_addr = std::mem::transmute(addr.octets());
                },
                IpAddr::V6(addr) => unsafe {
                    *row.NextHop.si_family_mut() = AF_INET6 as u16;
                    row.NextHop.Ipv6_mut().sin6_addr = std::mem::transmute(addr.octets());
                },
            }
        } else {
            // if we're not setting the gateway we need to explicitly set the family.
            unsafe {
                *row.NextHop.si_family_mut() = match route.destination {
                    IpAddr::V4(_) => AF_INET,
                    IpAddr::V6(_) => AF_INET6,
                } as u16;
            }
        }

        row.DestinationPrefix.PrefixLength = route.prefix;
        match route.destination {
            IpAddr::V4(addr) => unsafe {
                *row.DestinationPrefix.Prefix.si_family_mut() = AF_INET as u16;
                row.DestinationPrefix.Prefix.Ipv4_mut().sin_addr =
                    std::mem::transmute(addr.octets());
            },
            IpAddr::V6(addr) => unsafe {
                *row.DestinationPrefix.Prefix.si_family_mut() = AF_INET6 as u16;
                row.DestinationPrefix.Prefix.Ipv6_mut().sin6_addr =
                    std::mem::transmute(addr.octets());
            },
        }

        if let Some(metric) = route.metric {
            row.Metric = metric;
        }

        row
    }
}

use std::{io, net::IpAddr};
use winapi::shared::netioapi::CreateIpForwardEntry2;
use winapi::shared::winerror::ERROR_SUCCESS;
use winapi::shared::ws2def::{AF_INET, AF_INET6};
use winapi::shared::netioapi::{InitializeIpForwardEntry, MIB_IPFORWARD_ROW2};

use crate::Route;



pub(crate) struct Handle;

impl Handle {
    pub fn new() -> io::Result<Self> {
        Ok(Self)
    }

    pub(crate) async fn add(&self, route: &Route) -> io::Result<()> {
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
                IpAddr::V4(addr) => {
                    unsafe {
                        *row.NextHop.si_family_mut() = AF_INET as u16;
                        row.NextHop.Ipv4_mut().sin_addr = std::mem::transmute(addr.octets());
                    }
                }
                IpAddr::V6(addr) => {
                    unsafe {
                        *row.NextHop.si_family_mut() = AF_INET6 as u16;
                        row.NextHop.Ipv6_mut().sin6_addr = std::mem::transmute(addr.octets());
                    }
                }
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
            IpAddr::V4(addr) => {
                unsafe {
                    *row.DestinationPrefix.Prefix.si_family_mut() = AF_INET as u16;
                    row.DestinationPrefix.Prefix.Ipv4_mut().sin_addr = std::mem::transmute(addr.octets());
                }
            }
            IpAddr::V6(addr) => {
                unsafe {
                    *row.DestinationPrefix.Prefix.si_family_mut() = AF_INET6 as u16;
                    row.DestinationPrefix.Prefix.Ipv6_mut().sin6_addr = std::mem::transmute(addr.octets());
                }
            }
        }

        if let Some(metric) = route.metric {
            row.Metric = metric;
        }

        let err = unsafe { CreateIpForwardEntry2(&row) };
        if err != ERROR_SUCCESS {
            return Err(io::Error::new(io::ErrorKind::Other, format!("Error creating entry: {}", err)));
        }
        Ok(())
    }
}

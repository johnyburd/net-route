use net_route::Handle;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let handle = Handle::new()?;
    let routes = handle.list().await?;

    for route in routes {
        #[cfg(target_os = "linux")]
        println!(
            "{}/{} -> via {:?} dev {:?} src {:?}/{}",
            route.destination,
            route.prefix,
            route.gateway,
            route.ifindex,
            route.source,
            route.source_prefix,
        );
        #[cfg(not(target_os = "linux"))]
        println!(
            "{}/{} -> via {:?} dev {:?}",
            route.destination, route.prefix, route.gateway, route.ifindex,
        );
    }
    Ok(())
}

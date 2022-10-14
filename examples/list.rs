use net_route::Handle;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let handle = Handle::new()?;
    let routes = handle.list().await?;

    for route in routes {
        if route.destination.is_ipv6() {
            continue;
        }
        println!(
            "{}/{} -> via {:?} dev {:?}",
            route.destination, route.prefix, route.gateway, route.ifindex
        );
    }
    Ok(())
}

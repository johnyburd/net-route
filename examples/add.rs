use net_route::{Route, Handle};

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let handle = Handle::new()?;
    let route = Route::new("10.14.0.0".parse().unwrap(), 24)
        // windows options
        //.with_luid(19985273102270464)
        //.with_metric(5)
        .with_ifindex(9)
        .with_gateway("192.1.2.1".parse().unwrap());
    println!("route add {:?}", route);
    handle.add(&route).await
}
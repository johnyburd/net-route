
use net_route::Handle;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let handle = Handle::new()?;

    if let Some(route) = handle.default_route().await? {
        println!("Default route:\n{:?}", route);
    } else {
        println!("No default route found!");
    }
    Ok(())
}

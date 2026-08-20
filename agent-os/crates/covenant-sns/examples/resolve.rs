//! Live SNS resolution against the hosted resolver.
//! `NAME=bonfida.sol cargo run -p covenant-sns --example resolve`

use covenant_sns::{HttpSnsResolver, SnsResolver, DEFAULT_RESOLVER_URL};

#[tokio::main]
async fn main() {
    let name = std::env::var("NAME").unwrap_or_else(|_| "bonfida.sol".into());
    let resolver_url =
        std::env::var("COVENANT_SNS_RESOLVER_URL").unwrap_or_else(|_| DEFAULT_RESOLVER_URL.into());
    let r = HttpSnsResolver::new(&resolver_url);

    match r.resolve(&name).await {
        Ok(owner) => {
            println!("{name} -> {owner}");
            match r.reverse(&owner).await {
                Ok(domains) => {
                    println!("owner holds {} domain(s):", domains.len());
                    for d in domains.iter().take(10) {
                        println!("  {} ({})", d.domain, d.key);
                    }
                }
                Err(e) => eprintln!("reverse failed: {e}"),
            }
            match r.record(&name, "url").await {
                Ok(url) if !url.is_empty() => println!("url record: {url}"),
                Ok(_) => println!("url record: (none)"),
                Err(e) => eprintln!("url record: {e}"),
            }
        }
        Err(e) => eprintln!("resolve {name} failed: {e}"),
    }
}

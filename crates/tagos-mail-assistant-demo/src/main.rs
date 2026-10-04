mod mcp;
mod sqlite_store;

use std::env;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_ansi(false)
        .init();

    let mut args = env::args().skip(1);
    match args.next().as_deref() {
        Some("stdio") => mcp::serve_stdio(mcp::DemoMcp::new()).await,
        Some("bench") => {
            let rows = mcp::bench_report(40).await;
            let mut slow = false;
            for row in &rows {
                println!(
                    "{} p50={:.2}ms p95={:.2}ms",
                    row.name,
                    row.p50_us as f64 / 1000.0,
                    row.p95_us as f64 / 1000.0
                );
                if row.p95_us >= 500_000 {
                    slow = true;
                }
            }
            if slow {
                anyhow::bail!("a tool p95 is not under 500 ms");
            }
            Ok(())
        }
        Some("serve") | None => {
            let port = match args.next().as_deref() {
                Some("--port") => args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--port needs a number"))?
                    .parse::<u16>()?,
                Some(other) => anyhow::bail!("unknown argument {other}"),
                None => 8787,
            };
            let addr = format!("127.0.0.1:{port}");
            eprintln!("demo mcp listening on http://{addr}/mcp");
            mcp::serve(&addr, mcp::DemoMcp::new()).await
        }
        Some(other) => anyhow::bail!("unknown command {other}; use serve, stdio, or bench"),
    }
}

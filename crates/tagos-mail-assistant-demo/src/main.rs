mod auth;
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
            let mut port = 8787u16;
            let mut public_bind = false;
            let mut static_token = false;
            let mut profile: Option<&str> = None;
            let mut base_url: Option<String> = None;
            let mut listen: Option<String> = None;
            while let Some(arg) = args.next() {
                match arg.as_str() {
                    "--port" => {
                        port = args
                            .next()
                            .ok_or_else(|| anyhow::anyhow!("--port needs a number"))?
                            .parse()?;
                    }
                    "--public" => public_bind = true,
                    "--demo-static-token" => static_token = true,
                    "--auth-profile" => {
                        profile = Some(match args.next().as_deref() {
                            Some("mcp") => "mcp",
                            Some("alexa") => "alexa",
                            _ => anyhow::bail!("--auth-profile must be mcp or alexa"),
                        });
                    }
                    "--listen" => {
                        listen = Some(
                            args.next()
                                .ok_or_else(|| anyhow::anyhow!("--listen needs an address"))?,
                        );
                    }
                    "--base-url" => {
                        base_url = Some(
                            args.next()
                                .ok_or_else(|| anyhow::anyhow!("--base-url needs a URL"))?,
                        );
                    }
                    other => anyhow::bail!("unknown argument {other}"),
                }
            }
            let login = profile.is_some();
            let plan = auth::serve_plan(
                port,
                public_bind,
                login,
                static_token,
                base_url.as_deref(),
                listen.as_deref(),
            )
            .map_err(|err| anyhow::anyhow!(err))?;
            let mode = match profile {
                Some("alexa") => auth::AuthMode::Alexa,
                Some("mcp") => auth::AuthMode::Mcp,
                _ => auth::AuthMode::Off,
            };
            let state = if login {
                auth::AuthState::open(mode, plan.base.clone())
            } else {
                auth::AuthState::off()
            };
            if static_token {
                let token = state.issue_static_token();
                eprintln!("demo-static-token: {token}");
            }
            let addr = format!("{}:{port}", plan.bind_host);
            let app = mcp::DemoMcp::new();
            eprintln!("demo mcp listening on http://{addr}/mcp");
            mcp::serve(&addr, app, state).await
        }
        Some(other) => anyhow::bail!("unknown command {other}; use serve, stdio, or bench"),
    }
}

//! End-to-end example (including Cargo.toml) of a multi-modal app that can
//! either bring up a real HTTP server with Hyper (the default mode) or
//! serve itself via FastCGI, using a socket passed to it by the web server
//! that invoked it. In other words, you can give it its own server like normal,
//! OR you can throw it up onto shared hosting and forget about it.
use clap::Parser;
use tokio::net::TcpListener;
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt};

mod app;

#[derive(Parser)]
struct Cli {
    /// Serve in FastCGI mode, for low-touch hosting with mod_fcgid. Conflicts with --port.
    #[arg(long)]
    fcgi: bool,

    /// The TCP port to serve the app on. Defaults to 3000. Conflicts with --fcgi.
    #[arg(long)]
    port: Option<u16>,

    /// An alternate URI path to mount the app at, for shared domains. Use leading and
    /// trailing slash, like `/nested/`.
    #[arg(long, value_name = "PATH")]
    mount: Option<String>,
}

/// Word of warning about the Tokio multi-thread runtime: it defaults
/// to spawning a worker thread per logical CPU core. That's fine for a
/// single-purpose container or VM, but is NOT what you want in a
/// shared hosting environment. Here we're hardcoding a tame thread
/// limit, but in a real app you should skip the tokio::main macro and
/// make the cap configurable at run time.
#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    let args = Cli::parse();

    // validate and munge
    if args.fcgi && args.port.is_some() {
        panic!("The --fcgi and --port options are mutually exclusive. Choose one!");
    }
    let mount = args.mount.as_deref().unwrap_or("/");
    let port = args.port.unwrap_or(3000);

    // get app
    let dadapp = app::dadapp(mount);

    // Set up tracing
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "debug".into()),
        )
        .with(
            fmt::layer().with_timer(fmt::time::uptime()), // .with_writer(connect_to_log_socket),
        )
        .init();

    // blast off
    if args.fcgi {
        println!("Serving in fcgi mode, mounted at {}...", mount);
        busride_rs::serve_fcgid_with_graceful_shutdown(dadapp, 50.try_into().unwrap(), quit())
            .await
            .unwrap();
    } else {
        println!("Serving on port {}, mounted at {}...", port, mount);
        let listener = TcpListener::bind(("0.0.0.0", port)).await.unwrap();
        axum::serve(listener, dadapp)
            .with_graceful_shutdown(quit())
            .await
            .unwrap();
    }
    println!("Shutting down!");
}

/// Waits for a signal to shut the server down.
/// Taken directly from the fastcgi-server examples.
async fn quit() {
    use tokio::signal::unix::{signal, SignalKind};
    let Ok(mut term) = signal(SignalKind::terminate()) else {
        println!("Couldn't register SIGTERM handler; quitting immediately");
        return;
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            // don't care if ok or err
            println!("Received SIGINT (ctrl-c)");
        },
        _ = term.recv() => {
            println!("Received SIGTERM (kill)");
        },
    };
}

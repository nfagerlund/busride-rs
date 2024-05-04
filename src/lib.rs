//! A wrapper utility for serving an otherwise normal HTTP app over FastCGI,
//! an antique and awkward app server protocol that, despite its limitations,
//! enabled an ease of app deployment and maintenance that has yet to be
//! matched by modern tooling and infrastructure.
//!
//! The goal is to allow you to write normal, modern HTTP apps that default
//! to a conventional deployment mode, while enabling an *alternate* deployment
//! mode for self-hosting on a cheap shared web server. In other words, to
//! get the benefits of older hosting models without having to contort your
//! main app code around their oddities and limitations.
//!
//! This crate is in an experimental state, and currently only suppors the
//! [Axum](https://github.com/tokio-rs/axum) web framework... because that's
//! what I'm interested in using it with, and I couldn't justify the extra
//! work of generalizing it before even learning whether others are interested.
//! (Should be feasible, though.)
use bytes::BytesMut;
use fastcgi_server::async_io::Runner;
use fastcgi_server::{cgi, Config, ExitStatus};
use futures_util::AsyncWrite;
use futures_util::{io::BufWriter, AsyncWriteExt, FutureExt, StreamExt};
use std::future::Future;
use std::io;
use std::num::NonZeroUsize;
use std::os::fd::*;
use std::os::unix::fs::FileTypeExt;
use std::os::unix::net::UnixListener as StdUnixListener;
use tokio::net::UnixListener;
use tokio::sync::mpsc;
use tokio_util::codec::{BytesCodec, FramedRead};
use tokio_util::compat::{
    FuturesAsyncReadCompatExt, TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt,
};
use tower::Service;
use tracing::{debug, error, info, trace, Instrument};

// Shorthand types for working with fastcgi_server::async_io
type FcgiReader<'a> = tokio_util::compat::Compat<tokio::net::unix::ReadHalf<'a>>;
type FcgiWriter<'a> = tokio_util::compat::Compat<tokio::net::unix::WriteHalf<'a>>;
type FcgiRequest<'a, 'b, 'c> =
    fastcgi_server::async_io::Request<'a, FcgiReader<'b>, FcgiWriter<'c>>;

const FD_0_IS_TOO_NORMAL: &str = r#"Fatal error: wasn't executed by a compatible FastCGI client!
This server mode expects to be passed an open Unix socket on file descriptor 0,
rather than the normal stdin stream. The main modern client that supports
this is Apache's mod_fcgid."#;

#[derive(Debug)]
struct Fd0IsTooNormal;
impl std::fmt::Display for Fd0IsTooNormal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(FD_0_IS_TOO_NORMAL)
    }
}
impl std::error::Error for Fd0IsTooNormal {}

/// Like [`serve_fcgid_with_graceful_shutdown`], but punts on the graceful shutdown.
pub async fn serve_fcgid(app: axum::Router, max_connections: NonZeroUsize) -> io::Result<()> {
    let never = futures_util::future::pending::<()>();
    serve_fcgid_with_graceful_shutdown(app, max_connections, never).await
}

/// Serve an Axum app over FastCGI, listening on an already-open Unix domain socket
/// that was passed to the program on file descriptor 0 (in the slot where the
/// stdin handle should usually go). Apache2's optional `mod_fcgid` extension is
/// the last major client that knows how to start FastCGI servers on demand like
/// this, so it gets a shout-out in the function name.
///
/// Errors: In normal operation, this function just loops until the program is
/// terminated. An error return means we were unable to start listening on
/// our expected Unix socket, and never made it to the accept() loop.
pub async fn serve_fcgid_with_graceful_shutdown<F>(
    app: axum::Router,
    max_connections: NonZeroUsize,
    signal: F,
) -> io::Result<()>
where
    F: Future<Output = ()> + Send + 'static,
{
    // Verify that fd 0 is a unix socket before continuing.

    // SAFETY: We just want to do a metadata check on a file descriptor whose path on disk
    // we don't know... but there's no specific facility for that in std. The only way to
    // get metadata for an already open file like that is to wrap it in a File struct, but
    // for later code to be sound, we must ensure we never run its Drop impl. Hence using
    // a ManuallyDrop as an intermediate value.
    let fd_0_file_type = std::mem::ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(0) })
        .metadata()?
        .file_type();
    if !fd_0_file_type.is_socket() {
        eprintln!("{}", FD_0_IS_TOO_NORMAL);
        return Err(io::Error::other(Fd0IsTooNormal));
    }
    // SAFETY: Yes, it is unsafe to pick a raw file descriptor up off the ground and lick it.
    // But, we verified above that it's what we expect it to be.
    let std_listener = unsafe { StdUnixListener::from_raw_fd(0) };

    // Set up tokio UnixListener
    std_listener.set_nonblocking(true)?;
    let listener = UnixListener::from_std(std_listener)?;
    let local_addr = listener.local_addr()?;
    info!(protocol = "unix", ?local_addr, "listener created");

    // Build fastcgi-server config and runner
    let config = Config::with_conns(max_connections);
    let runner = config.async_runner();

    // Loop to accept connections and serve
    tokio::select! {
        biased;  // poll in order, so check the cancel future first
        _ = signal => {},
        _ = serve_loop(&runner, app, listener) => {}, // runs forever
    };

    // Gracefully shut down
    runner.shutdown().await;
    Ok(())
}

/// Perform the main accept-and-serve loop for translating FastCGI requests to
/// app-level HTTP requests (and back again).
async fn serve_loop(runner: &Runner, app: axum::Router, listener: UnixListener) {
    // Loop to accept connections and serve
    loop {
        let token = runner.get_token().await;
        match listener.accept().await {
            Err(e) => {
                error!(protocol = "unix", "accept failed: {}", &e);
                continue;
            }
            Ok((mut connection, _)) => {
                // Tracing span for the task that'll handle this connection
                let span = tracing::error_span!("fastcgi_connection", protocol = "unix",);
                // Good thing Axum apps are cheap to clone, cuz we need several.
                // This one belongs to the connection, which might serve several requests.
                let app_for_conn = app.clone();

                // Spawn a separate task to handle this connection
                tokio::spawn(
                    async move {
                        debug!("new connection accepted on dedicated task");
                        let (t_r, t_w) = connection.split();
                        // Tokio's UnixStream uses Tokio's Async IO traits; convert that to
                        // the futures_util::io traits that fastcgi-server uses.
                        let r = t_r.compat();
                        let w = t_w.compat_write();
                        // Then, handle the connection! The handler might get called several
                        // times, so it performs its own additional clone of the app.
                        token
                            .run(r, w, move |r| {
                                handle_fcgi_request_with_axum_app(app_for_conn.clone(), r).boxed()
                            })
                            .await
                    }
                    .instrument(span),
                );
            }
        }
    }
}

/// Translates an incoming FastCGI request to an HTTP request, handles it with the
/// provided Axum app, and sends the result back to the client as a FastCGI response.
/// This all happens in one function, because fastcgi_server::async_io::Request is
/// a hefty beast that also includes a response writer handle. This function is
/// meant to be called in the handler closure passed to Token::run().
async fn handle_fcgi_request_with_axum_app(
    mut app: axum::Router,
    req: &mut FcgiRequest<'_, '_, '_>,
) -> std::io::Result<ExitStatus> {
    // About that return type: it's tied to both the CGI programming model and the
    // FastCGI network protocol.
    //
    // - An exit code of 0 means we successfully handled the request. We might have
    //   successfully handled it with an HTTP 4xx error, but we handled it!
    // - A non-0 exit code means we failed at handling the *request,* but we have no
    //   reason to think the connection's borked. Fcgi connections can be re-used for
    //   multiple requests.
    // - An io::Error means the connection is hosed and the client needs to start over.
    //
    // mod_fcgid can't usefully distinguish exit codes other than 0, so mostly you'll
    // set up a tracing fmt subscriber and rely on the fact that stdout ends up in
    // Apache's error_log.

    // FastCGI's programming model had several roles, but we only care about "responder".
    if req.role() != fastcgi_server::protocol::Role::Responder {
        error!(
            blame = "end user",
            "App received a request for a non-Responder role; the client must be misconfigured"
        );
        return Ok(ExitStatus::Complete(1));
    }

    // This ensures we can both access the request input stream and write to the output
    // stream. Semantics are somewhat different for non-Responder roles, but we don't care.
    req.writeable().await?;

    // Construct an http::Request for our inner app
    let (http_req, body_tx) = match http_request_from_fcgi_request(req) {
        Ok(stuff) => stuff,
        Err(e) => {
            // This means the http headers, URI, or method failed to parse.
            error!(
                blame = "apache, fastcgi-server, or nick",
                "Failed to finalize http::Request: {}", e
            );
            return Ok(ExitStatus::Complete(1));
        }
    };
    trace!("Constructed http request");

    // Grab the output handle early, before we borrow req as mut for an extended read
    let w = req.output_stream(fastcgi_server::protocol::RecordType::Stdout);

    // well, I'd like to just ::spawn the body transmission, but it has borrowed
    // data that I don't want to copy. So!

    // Stream the decoded request body into the HTTP request
    let body_tx_fut = async {
        trace!("Started polling body transmit future");
        // Or I could stack-allocate a fixed-size buffer and loop on
        // poll_read. But what I'm banking on here is that the tokio_stream/_util authors
        // know more than me about how to cheat their way out of copies.
        let mut bytes_stream = FramedRead::new(req.compat(), BytesCodec::new());
        while let Some(x) = bytes_stream.next().await {
            trace!("streaming bytes...");
            if let Err(e) = body_tx.send(x) {
                // I think this can happen if the axum app detects something wrong with the
                // request before it finishes slurping the body, and decides to just bail;
                // for example, route's got a Json() extractor but the incoming content-type
                // is wrong. So, we'll log an error event here, but allow the app to finish
                // responding with whatever its actual complaint was.
                error!(
                    blame = "end user or app",
                    "Body bytes receiver got dropped, probably bc the app didn't want any: {}", e
                );
                break;
            };
        }
        // Once the send loop is done, gotta explicitly drop the transmitter so that
        // the stream on the other side knows we're done.
        drop(body_tx);
    };

    // Actually call our inner HTTP app!
    let app_response_fut = app.call(http_req);

    // Since routes can extract a completed body before they start to return a response,
    // we now need to await these two futures in tandem.
    trace!("Polling body stream and app futures in tandem:");
    let (_, app_response) = tokio::join!(body_tx_fut, app_response_fut);
    trace!("successfully finished polling joint futures, received app response");
    // neat can't-panic unwrap trick for Infallible, from the axum repo's examples
    let app_response = match app_response {
        Ok(x) => x,
        Err(e) => match e {},
    };

    let mut buffered = BufWriter::new(w);
    // If this write hits an error we literally can't write output anymore,
    // so probably the connection's hosed; return an io::Error instead of an exit code.
    trace!("writing app response as fcgi response");
    write_http_response(&mut buffered, app_response).await?;

    // ok, done!
    buffered.flush().await?;
    trace!("finished writing fcgi response and flushing output");

    Ok(ExitStatus::SUCCESS)
}

/// Build an http::Request with a streaming body, and return it along with
/// a sender handle for streaming bytes into the body.
///
/// Errors: Returns an error if the resulting HTTP request wasn't valid,
/// probably because the headers failed to parse; this probably means a bug in
/// either fastcgi-server or the fastcgi client that sent the original request.
fn http_request_from_fcgi_request(
    req: &mut FcgiRequest<'_, '_, '_>,
) -> Result<
    (
        http::Request<axum::body::Body>,
        mpsc::UnboundedSender<std::io::Result<BytesMut>>,
    ),
    http::Error,
> {
    // About HTTP version: the web server might be speaking whatever, and
    // cgi::SERVER_PROTOCOL will tell the truth about it. But over here
    // across the fastcgi barrier, it's gonna ACT like h1 no matter what.
    let mut h_req = http::Request::builder()
        .version(http::Version::HTTP_11)
        .method(req.get_var(cgi::REQUEST_METHOD).unwrap_or(b"GET"))
        .uri(req.get_var(cgi::REQUEST_URI).unwrap_or(b"/"));
    // Special headers: content-type and content-length aren't prefixed w/ HTTP_
    if let Some(v) = req.get_var(cgi::CONTENT_TYPE) {
        h_req = h_req.header("Content-Type", v);
    }
    if let Some(v) = req.get_var(cgi::CONTENT_LENGTH) {
        h_req = h_req.header("Content-Length", v);
    }
    // But the rest of the headers all became vars prefixed w/ HTTP_
    h_req = req.env_iter().fold(h_req, |memo, (k, v)| {
        if k.as_ref().starts_with("HTTP_") {
            let h = &k.as_ref()[5..];
            // don't sweat the allcaps, http crate doesn't mind
            memo.header(h, v)
        } else {
            memo
        }
    });

    // We use a channel, because the body needs an owned value as its stream.
    // I'm using Unbounded, because... well, mostly because I'm Baby. I *suspect*
    // Bounded is more correct, but I couldn't reason out what message limit
    // would do the right thing with the BytesCodec we're using in the caller.
    // LMK if you know why to use Bounded and what number to give it. 🌻
    let (body_tx, body_rx) = mpsc::unbounded_channel();

    let rx_stream = tokio_stream::wrappers::UnboundedReceiverStream::new(body_rx);
    let stream_body = axum::body::Body::from_stream(rx_stream);
    h_req.body(stream_body).map(|b| (b, body_tx))
}

/// Use a provided http::Response to write a CGI/1.1 response to the provided AsyncWriter.
async fn write_http_response(
    out: impl AsyncWrite,
    resp: http::Response<axum::body::Body>,
) -> std::io::Result<()> {
    tokio::pin!(out);

    // TODO: there's probably a good way to dump these headers directly into the
    // buffered AsyncWrite without the extra sync copy, but it doesn't seem urgent rn.
    let mut response_headers_bytes: Vec<u8> = Vec::new();
    cgi::response::http_headers(&mut response_headers_bytes, &resp)?;
    trace!("writing fcgi response headers...");
    out.write_all(&response_headers_bytes).await?;
    trace!("done writing fcgi response headers");

    // Response body can become a stream of Bytes
    let mut body_stream = resp.into_body().into_data_stream();
    trace!("starting to write fcgi response body");
    while let Some(maybe_hunk) = body_stream.next().await {
        match maybe_hunk {
            Ok(hunk) => {
                trace!("writing bytes...");
                // Bytes does a Deref to [u8], so
                out.write_all(&hunk).await?;
            }
            Err(e) => {
                // Literally couldn't write what we wanted to the output stream, so
                // return Err and make em start a new connection.
                error!(blame = "app", "Error reading reponse body from app: {}", e);
                return Err(std::io::Error::other(e));
            }
        }
    }
    trace!("finished writing fcgi response body");

    Ok(())
}

# Busride-rs

Low-fuss, free-range, shared hosting deployment for [Axum](https://github.com/tokio-rs/axum) apps, by wrapping them in [fastcgi-server](https://github.com/TheJokr/fastcgi-server) and letting [Apache's `mod_fcgid`][mod_fcgid] do the driving. Put your app on the bus and wave goodbye.

[mod_fcgid]: https://httpd.apache.org/mod_fcgid/mod/mod_fcgid.html

## Pro-tip: Don't

- 🚨 This is unsupported experimental code. 🚨
- Its [primary dependency](https://github.com/TheJokr/fastcgi-server) is not even released on crates.io.
- I don't have a testing strategy for this yet.
- Does your web host even have [`mod_fcgid`][mod_fcgid] enabled? Better check and make sure.
- FastCGI is antique tech that is not especially high-performance by modern standards. Hyper with h2 is gonna treat u better.
- You never know what's gonna happen on the bus! Maybe your app will buy weed from a 10th-grader. Maybe the metalheads who sit in the back will get your app way too into D&D.
- If you proceed past this point, you're kind of asking for trouble.

OK, now that that's out of the way,

## How to Use It

Currently, this library requires the use of Axum, Tokio, and (on the Apache side) `mod_fcgid`.

There's an [end-to-end example](./examples/dadjoke) in the examples folder.

### Writing Your App

Normal Axum apps should work largely unchanged, although websockets likely aren't possible. Just give your app an option or setting to determine whether it should attempt FastCGI mode, and use that to decide whether to call `busride_rs::serve_fcgid` instead of the standard `axum::serve`.

Make sure your app doesn't make any assumptions about the cwd where it is invoked, because you won't have control over that. Anything you need from disk, you'll need to reference explicitly through config or CLI options.

### Configuring `mod_fcgid`

First off, make sure the Apache user is able to traverse to and execute your app binary.

Next: This translation layer is intended to be used with [Apache's `mod_fcgid`][mod_fcgid], and you'll need to configure the following directives for the location where you want to serve your app:

- `Options +ExecCGI -Indexes`
- `SetHandler fcgid-script`
- `FcgidWrapper "/path/to/command --opts" virtual`
    - The `virtual` suffix allows the app to handle all routing of URL paths within its area of control, without the web server first checking that files corresponding to the requested paths are present. You always want this.

Since this is intended for use in shared hosting where you don't control the top-level Apache configs, you'll probably need to put those in an `.htaccess` file in the directory associated with your target URI path.

(If you **do** control the whole server, don't even bother with this crate!! Just run all your apps as persistent services on different local ports and use a top-level reverse-proxy to route between em. If you still want the auto-nap/wake lifecycle (and I can't blame ya), maybe look into systemd socket-activated-services.)

### Done!

Once the handler/wrapper is configured, Apache should automatically start your app process when it gets requests for the URLs it controls, and should keep it alive as long as it's receiving steady traffic. It can and will shut your process down if it thinks it doesn't urgently need it (the server owner gets to tune this to their liking), but it'll bring you right back when called on.

### Deploying Upgrades

You'll generally want to `killall your-command` after installing a new version of your binary, because the web server might still be running an instance of the old one.

## Faq

### Why are you doing this?

I'm working on a blog post to explain this in more detail, but the short answer is in [this little 3m video demo](https://www.youtube.com/watch?v=1OIrQsrYVds) I recorded at an earlier stage of the project. I can deploy an arbitrary number of low-traffic and unpopular apps onto existing infrastructure with zero marginal cost and zero ongoing maintenance, and frankly you either get it or you don't.

### OK but why FastCGI tho

_The street finds its own uses for things._

The core concept behind classical shared hosting is that you don't get to daemonize anything, but you can still run server-side applications as long as you leave the web server in charge of running (and terminating) their code whenever it sees fit.

Honestly that seems fine to me! At least for low-traffic personal services! All I really wanted was a way to associate an arbitrary HTTP-serving process with a given domain or URL path. It's easy to imagine a slick, generalized way to accomplish this; or at least, I have ideas.

But the slick, generalized, HTTP-native way to do that doesn't exist. Fcgi, on the other hand, was just sitting there, already enabled by default on my host.

### What's actually the deal with FastCGI?

FastCGI is a client/server network protocol that can run in two primary modes:

- Run a permanent FastCGI server at a stable, well-known address, and point your web server at that address.
    - PHP-FPM runs like this... and since it's the only really _important_ FastCGI app, this is the only mode supported by most FastCGI clients, including Nginx, Caddy, and Apache's built-in `mod_proxy_fcgi`.
- Point your web server at an app server binary, and it will invoke it on-demand as a semi-persistent process, passing it an already-open Unix socket on file descriptor 0 (i.e., where stdin should to be).

That second mode is what we want!

It turns out that `mod_fcgid` (not included in Apache 2.4 by default, but still maintained as an add-on) uses the old-style process management model, and it is more than happy to serve a compiled Rust app on demand.

Again, FastCGI itself has no real benefits, and if there was a way to have my web host start and kill an HTTP app server according to demand, I'd definitely use that instead. But for the time being, no dice.

### Nice.

My friend Robert suggested that I name this thing N.I.C.E. (Nick's Inverted CGI Endpoint) and there's still a chance I might change it to that.

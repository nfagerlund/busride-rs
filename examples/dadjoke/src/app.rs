use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::get,
    Json, Router,
};
use serde::Deserialize;
use std::sync::atomic::{AtomicU32, Ordering};

#[derive(Clone)]
struct DadState {
    counter: &'static AtomicU32,
    mount_path: String,
}

/// Creates a new instance of dad app, to be mounted at the specified URI path.
/// Mount paths should start with /; for a default mount at the root of the domain,
/// pass either "/" or "".
/// Doing this optional mount thing is honestly a little wonky, because Axum's
/// Path extractors always get the whole path without de-nesting, so you have
/// to thread handling for it through all your routes. But it makes a neat demo,
/// and I can imagine a use for it IRL, fussy or no.
pub fn dadapp(mount_path: &str) -> Router {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let state = DadState {
        counter: &COUNTER,
        mount_path: mount_path
            .strip_prefix('/')
            .unwrap_or(mount_path)
            .to_string(),
    };
    let app = Router::new()
        .route("/*allyall", get(dad))
        .route("/", get(rooty).post(post_dad))
        .with_state(state);
    if mount_path == "/" || mount_path.is_empty() {
        app
    } else {
        Router::new().nest(mount_path, app)
    }
}

/// GET handler for main app route, which consumes the entire URI path beyond the root.
async fn dad(
    Path(path): Path<String>,
    State(state): State<DadState>,
) -> Result<String, StatusCode> {
    let known_visits = state.counter.fetch_add(1, Ordering::Relaxed) + 1;
    let name = path.strip_prefix(&state.mount_path).unwrap_or(&path);
    Ok(format!(
        "Hi {}, i'm dad\n\n{} dads joked so far this lifetime",
        name, known_visits,
    ))
}

/// Little GET handler at the root to confirm that it's mounted where you expect it to be.
async fn rooty() -> Result<String, StatusCode> {
    Ok("wow you made it to the root".to_string())
}

/// The body of a dad-POST.
#[derive(Deserialize)]
struct DadBod {
    name: String,
}

/// POST handler at the root of the site, providing an alternate way to get dunked on.
/// Expects application/json bodies of the form `{"name":"name goes here"}`.
async fn post_dad(
    State(state): State<DadState>,
    Json(dad_bod): Json<DadBod>,
) -> Result<String, StatusCode> {
    let known_visits = state.counter.fetch_add(1, Ordering::Relaxed) + 1;
    Ok(format!(
        "Hi {}, i'm POST_DAD 📬👨\n\n{} dads joked so far this lifetime",
        &dad_bod.name, known_visits,
    ))
}

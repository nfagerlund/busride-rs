fn main() {}

mod app {
    use axum::{
        extract::{Path, State},
        http::StatusCode,
        routing::get,
        Json, Router,
    };
    use serde::Deserialize;
    use std::sync::atomic::{AtomicU32, Ordering};

    async fn dad(
        Path(path): Path<String>,
        State(counter): State<&AtomicU32>,
    ) -> Result<String, StatusCode> {
        let known_visits = counter.fetch_add(1, Ordering::Relaxed) + 1;
        Ok(format!(
            "Hi {}, i'm dad\n\n{} dads joked so far this lifetime",
            &path, known_visits,
        ))
    }

    async fn rooty() -> Result<String, StatusCode> {
        Ok("wow you made it to the root".to_string())
    }

    #[derive(Deserialize)]
    struct DadBod {
        name: String,
    }

    async fn post_dad(
        State(counter): State<&AtomicU32>,
        Json(dad_bod): Json<DadBod>,
    ) -> Result<String, StatusCode> {
        let known_visits = counter.fetch_add(1, Ordering::Relaxed) + 1;
        Ok(format!(
            "Hi {}, i'm POST_DAD 📬👨\n\n{} dads joked so far this lifetime",
            &dad_bod.name, known_visits,
        ))
    }

    pub fn dumbapp() -> Router {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        Router::new()
            .route("/*allyall", get(dad))
            .route("/", get(rooty).post(post_dad))
            .with_state(&COUNTER)
    }
}

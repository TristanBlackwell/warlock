use anyhow::Context;

mod app;
mod handlers;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let app = app::create_app();

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000")
        .await
        .context("Unable to bind TCP listener.")?;

    axum::serve(listener, app)
        .await
        .context("Failed to serve axum.")?;

    Ok(())
}

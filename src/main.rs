mod app;
mod bot;
mod config;
mod db;
mod models;
mod telemetry;
mod util;

use anyhow::Result;
use teloxide::prelude::Bot;
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
  telemetry::init()?;
  let config = config::Config::from_env()?;
  let admin_count = config.admins.len();
  info!(admin_count = admin_count, "starting bot");

  let bot = Bot::new(config.bot_token.clone());
  let db = db::Db::connect(&config.database_url).await?;
  let app = app::App::new(bot, db, config.admins);
  app.run().await
}

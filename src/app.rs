use std::sync::Arc;

use teloxide::dispatching::UpdateHandler;
use teloxide::dispatching::dialogue::InMemStorage;
use teloxide::dptree;
use teloxide::prelude::*;

use crate::bot;
use crate::bot::AppContext;
use crate::bot::DialogueStorage;
use crate::db::Db;

pub struct App {
  bot: Bot,
  context: Arc<AppContext>,
  handler: UpdateHandler<anyhow::Error>,
}

impl App {
  pub fn new(bot: Bot, db: Db, admins: Vec<i64>) -> Self {
    let context = Arc::new(AppContext::new(db, admins));
    let handler = bot::build_schema();
    Self { bot, context, handler }
  }

  pub async fn run(self) -> anyhow::Result<()> {
    let storage: Arc<DialogueStorage> = InMemStorage::new();

    let me = self.bot.get_me().await?;

    Dispatcher::builder(self.bot.clone(), self.handler)
      .dependencies(dptree::deps![self.context.clone(), storage.clone(), me])
      .enable_ctrlc_handler()
      .build()
      .dispatch()
      .await;

    Ok(())
  }
}

use std::collections::HashSet;
use std::sync::Arc;

use anyhow::Context;
use anyhow::Result;
use sqlx::Error as SqlxError;
use teloxide::ApiError;
use teloxide::RequestError;
use teloxide::dispatching::UpdateHandler;
use teloxide::dispatching::dialogue::Dialogue;
use teloxide::dptree;
use teloxide::prelude::*;
use teloxide::types::CallbackQuery;
use teloxide::types::ChatId;
use teloxide::types::FileId;
use teloxide::types::InlineKeyboardButton;
use teloxide::types::InlineKeyboardMarkup;
use teloxide::types::InputFile;
use teloxide::types::InputMedia;
use teloxide::types::InputMediaPhoto;
use teloxide::types::Message;
use teloxide::types::MessageEntity;
use teloxide::types::MessageId;
use teloxide::types::ParseMode;
use teloxide::types::User;
use teloxide::utils::command::BotCommands;
use teloxide::utils::markdown;
use thiserror::Error;
use tracing::info;
use tracing::instrument;
use tracing::warn;

use crate::bot::Command;
use crate::bot::DialogueStorage;
use crate::bot::HandlerResult;
use crate::bot::context::AppContext;
use crate::bot::state::AddItemDraft;
use crate::bot::state::BidDraft;
use crate::bot::state::ConversationState;
use crate::bot::state::DraftStage;
use crate::models::CategoryRow;
use crate::models::ItemRow;
use crate::util::MoneyError;
use crate::util::format_cents;
use crate::util::parse_money_to_cents;

type SharedContext = Arc<AppContext>;
type BotDialogue = Dialogue<ConversationState, DialogueStorage>;

const MAIN_MENU_TEXT: &str = "🤖 What would you like to do?";
const MEDIA_GROUP_BATCH: usize = 10;

pub fn build_schema() -> UpdateHandler<anyhow::Error> {
  let message_handler = Update::filter_message()
    .enter_dialogue::<Message, DialogueStorage, ConversationState>()
    .branch(command_branch())
    .branch(dptree::case![ConversationState::AddItem(draft)].endpoint(handle_additem_message))
    .branch(dptree::case![ConversationState::PlaceBid(draft)].endpoint(handle_bid_message))
    .branch(dptree::case![ConversationState::AddCategory { admin_tg_id }].endpoint(handle_add_category_message))
    .branch(dptree::case![ConversationState::CloseItem { admin_tg_id }].endpoint(handle_close_item_message))
    .branch(dptree::case![ConversationState::RemoveItem { admin_tg_id }].endpoint(handle_remove_item_message))
    .branch(dptree::case![ConversationState::RemoveCategory { admin_tg_id }].endpoint(handle_remove_category_message))
    .branch(dptree::case![ConversationState::Broadcast { admin_tg_id }].endpoint(handle_broadcast_message))
    .branch(dptree::endpoint(handle_idle_text));

  let callback_handler = Update::filter_callback_query()
    .enter_dialogue::<CallbackQuery, DialogueStorage, ConversationState>()
    .endpoint(handle_callback_query);

  dptree::entry().branch(message_handler).branch(callback_handler)
}

fn command_branch() -> UpdateHandler<anyhow::Error> {
  dptree::entry()
    .filter_command::<Command>()
    .branch(dptree::case![Command::Start].endpoint(handle_start))
    .branch(dptree::case![Command::Help].endpoint(handle_help))
}

#[instrument(skip(bot, ctx, dialogue, msg))]
async fn handle_start(bot: Bot, dialogue: BotDialogue, ctx: SharedContext, msg: Message) -> HandlerResult {
  dialogue.reset().await?;
  let user = msg.from.as_ref().context("message missing sender")?;
  ensure_user_record(&ctx, user).await?;
  let user_id = user.id.0 as i64;
  let username = user.username.as_deref().unwrap_or("-");
  info!(user_id, chat_id = %msg.chat.id, username, "received /start command");
  send_main_menu_message(&bot, &ctx, msg.chat.id, user_id).await
}

#[instrument(skip(bot, msg))]
async fn handle_help(bot: Bot, msg: Message) -> HandlerResult {
  info!(chat_id = %msg.chat.id, "received /help command");
  let mut text = Command::descriptions().to_string();
  text.push_str(
    "\n\nAll auction features are available from the on-screen menu buttons. Use /start to open the menu again.",
  );
  bot.send_message(msg.chat.id, text).await?;
  Ok(())
}

#[instrument(skip(bot, ctx))]
async fn send_main_menu_message(bot: &Bot, ctx: &SharedContext, chat: ChatId, user_id: i64) -> HandlerResult {
  bot
    .send_message(chat, MAIN_MENU_TEXT)
    .reply_markup(main_menu_keyboard(ctx, user_id))
    .await?;
  info!(user_id, chat_id = %chat, "sent main menu message");
  Ok(())
}

#[instrument(skip(bot, ctx))]
async fn show_main_menu(
  bot: &Bot,
  ctx: &SharedContext,
  chat: ChatId,
  message_id: MessageId,
  user_id: i64,
) -> HandlerResult {
  let keyboard = main_menu_keyboard(ctx, user_id);
  let request = bot
    .edit_message_text(chat, message_id, MAIN_MENU_TEXT)
    .reply_markup(keyboard);
  match request.await {
    Ok(_) => info!(user_id, chat_id = %chat, message_id = %message_id, "updated main menu message"),
    Err(RequestError::Api(ApiError::MessageNotModified)) => {
      info!(user_id, chat_id = %chat, message_id = %message_id, "main menu message already current");
      return Ok(());
    },
    Err(err) => return Err(err.into()),
  }
  Ok(())
}

fn main_menu_keyboard(ctx: &SharedContext, user_id: i64) -> InlineKeyboardMarkup {
  let mut rows = vec![vec![InlineKeyboardButton::callback(
    "🗂️ Catalogue",
    "menu:catalogue".to_string(),
  )]];

  rows.push(vec![
    InlineKeyboardButton::callback("🪙 My bids", "menu:my_bids".to_string()),
    InlineKeyboardButton::callback("⭐ My favorites", "menu:favorites".to_string()),
  ]);

  rows.push(vec![InlineKeyboardButton::callback(
    "⚙️ My settings",
    "menu:settings".to_string(),
  )]);

  if ctx.is_admin(user_id) {
    rows.push(vec![InlineKeyboardButton::callback(
      "🛡️ Admin panel",
      "menu:admin".to_string(),
    )]);
  }

  InlineKeyboardMarkup::new(rows)
}

fn admin_menu_keyboard() -> InlineKeyboardMarkup {
  InlineKeyboardMarkup::new(vec![
    vec![
      InlineKeyboardButton::callback("🆕 Add category", "admin:add_category".to_string()),
      InlineKeyboardButton::callback("📦 Add item", "admin:add_item".to_string()),
    ],
    vec![
      InlineKeyboardButton::callback("🗑 Remove item", "admin:remove_item".to_string()),
      InlineKeyboardButton::callback("🗑 Remove category", "admin:remove_category".to_string()),
    ],
    vec![
      InlineKeyboardButton::callback("🛑 Close item", "admin:close_item".to_string()),
      InlineKeyboardButton::callback("📢 Broadcast", "admin:broadcast".to_string()),
    ],
    vec![InlineKeyboardButton::callback(
      "🔔 Notify new lots",
      "admin:notify_new".to_string(),
    )],
    vec![InlineKeyboardButton::callback("⬅️ Main menu", "menu:root".to_string())],
  ])
}

fn main_menu_only_keyboard() -> InlineKeyboardMarkup {
  InlineKeyboardMarkup::new(vec![vec![InlineKeyboardButton::callback(
    "⬅️ Main menu",
    "menu:root".to_string(),
  )]])
}

fn settings_menu_keyboard(notifications_disabled: bool) -> InlineKeyboardMarkup {
  let toggle_label = if notifications_disabled {
    "🔔 Enable updates"
  } else {
    "🔕 Mute updates"
  };

  InlineKeyboardMarkup::new(vec![
    vec![InlineKeyboardButton::callback(
      toggle_label.to_string(),
      "settings:toggle_notifications".to_string(),
    )],
    vec![InlineKeyboardButton::callback("⬅️ Main menu", "menu:root".to_string())],
  ])
}

#[instrument(skip(bot, ctx))]
async fn show_catalogue_menu(bot: &Bot, ctx: &SharedContext, chat: ChatId, message_id: MessageId) -> HandlerResult {
  update_categories_menu(bot, ctx, chat, message_id).await
}

#[instrument(skip(bot))]
async fn show_admin_menu(bot: &Bot, chat: ChatId, message_id: MessageId) -> HandlerResult {
  let request = bot
    .edit_message_text(chat, message_id, "🛡️ Admin panel\n\nChoose an action:")
    .reply_markup(admin_menu_keyboard());
  match request.await {
    Ok(_) => info!(chat_id = %chat, message_id = %message_id, "updated admin menu"),
    Err(RequestError::Api(ApiError::MessageNotModified)) => {
      info!(chat_id = %chat, message_id = %message_id, "admin menu already current");
      return Ok(());
    },
    Err(err) => return Err(err.into()),
  }
  Ok(())
}

#[instrument(skip(bot, ctx))]
async fn show_settings_menu(
  bot: &Bot,
  ctx: &SharedContext,
  chat: ChatId,
  message_id: MessageId,
  user_id: i64,
) -> HandlerResult {
  let notifications_disabled = ctx.db().notifications_disabled(user_id).await?;
  let status_line = if notifications_disabled {
    "🔕 Notifications are OFF"
  } else {
    "🔔 Notifications are ON"
  };
  let hint_line = "Toggle below to control auction updates.";
  let request = bot
    .edit_message_text(
      chat,
      message_id,
      format!("⚙️ Settings\n\n{}\n{}", status_line, hint_line),
    )
    .reply_markup(settings_menu_keyboard(notifications_disabled));
  match request.await {
    Ok(_) => info!(chat_id = %chat, message_id = %message_id, "updated settings menu"),
    Err(RequestError::Api(ApiError::MessageNotModified)) => {
      info!(chat_id = %chat, message_id = %message_id, "settings menu already current");
      return Ok(());
    },
    Err(err) => return Err(err.into()),
  }
  Ok(())
}

#[instrument(skip(bot, ctx))]
async fn send_favorites_list(bot: &Bot, ctx: &SharedContext, chat: ChatId, user_id: i64) -> HandlerResult {
  let favorites = ctx.db().list_favorites(user_id).await?;

  if favorites.is_empty() {
    info!(user_id, chat_id = %chat, "no favorites to display");
    bot.send_message(chat, "⭐ No favorites yet.").await?;
    return Ok(());
  }

  info!(user_id, chat_id = %chat, count = favorites.len(), "sending favorites list");
  bot
    .send_message(chat, format!("⭐ Favorites ({}):", favorites.len()))
    .await?;

  for item in favorites {
    if !send_item(bot, ctx, chat, item.id, Some(user_id)).await? {
      warn!(item_id = item.id, "favorite item missing while rendering");
    }
  }

  Ok(())
}

#[instrument(skip(bot, ctx))]
async fn send_my_bids_list(bot: &Bot, ctx: &SharedContext, chat: ChatId, user_id: i64) -> HandlerResult {
  let bids = ctx.db().list_user_bid_items(user_id).await?;

  if bids.is_empty() {
    info!(user_id, chat_id = %chat, "no bids to display");
    bot.send_message(chat, "🪙 You have not placed any bids yet.").await?;
    return Ok(());
  }

  info!(user_id, chat_id = %chat, count = bids.len(), "sending bid summary");
  bot
    .send_message(chat, format!("🪙 Active bids ({} items):", bids.len()))
    .await?;

  for (item, _) in bids {
    if !send_item(bot, ctx, chat, item.id, Some(user_id)).await? {
      warn!(item_id = item.id, "bid item missing while rendering");
    }
  }

  Ok(())
}

#[instrument(skip(bot, ctx, dialogue, msg, draft))]
async fn handle_additem_message(
  bot: Bot,
  dialogue: BotDialogue,
  ctx: SharedContext,
  msg: Message,
  mut draft: AddItemDraft,
) -> HandlerResult {
  let user = msg.from.as_ref().context("message missing sender")?;
  if user.id.0 as i64 != draft.seller_tg_id {
    bot
      .send_message(
        msg.chat.id,
        "Only the admin who started this item creation can respond.",
      )
      .await?;
    return Ok(());
  }

  let mut added_photo = false;
  if let Some(photo) = msg.photo().and_then(|photos| photos.last())
    && !draft.image_file_ids.iter().any(|existing| existing == &photo.file.id)
  {
    draft.image_file_ids.push(photo.file.id.clone());
    added_photo = true;
  }

  let text = message_text(&msg).map(|t| t.trim()).filter(|t| !t.is_empty());
  let chat_id = msg.chat.id;
  info!(
    seller_id = draft.seller_tg_id,
    chat_id = %chat_id,
    stage = ?draft.stage,
    "handling add item input"
  );

  if text.is_none() {
    dialogue.update(ConversationState::AddItem(draft.clone())).await?;
    if added_photo {
      bot
        .send_message(
          chat_id,
          format!("🖼️ Added photo. Total uploaded: {}.", draft.image_file_ids.len()),
        )
        .await?;
      info!(
        seller_id = draft.seller_tg_id,
        chat_id = %chat_id,
        total_photos = draft.image_file_ids.len(),
        "stored new draft photo"
      );
    }
    return Ok(());
  }

  if matches!(text, Some(value) if value.eq_ignore_ascii_case("cancel")) {
    dialogue.reset().await?;
    bot.send_message(chat_id, "❌ Item creation cancelled.").await?;
    return Ok(());
  }

  match draft.stage {
    DraftStage::Category => {
      let Some(name) = text else {
        bot.send_message(chat_id, "🗂️ Please provide a category name.").await?;
        return Ok(());
      };
      let (category, _) = ensure_category(&ctx, name).await?;
      draft.category_id = Some(category.id);
      draft.category_name = Some(category.name);
      draft.stage = DraftStage::Title;
      dialogue.update(ConversationState::AddItem(draft)).await?;
      bot.send_message(chat_id, "📝 Enter item title:").await?;
    },
    DraftStage::Title => {
      let Some(title) = text else {
        bot.send_message(chat_id, "📝 Please provide a title.").await?;
        return Ok(());
      };
      draft.title = Some(title.to_string());
      draft.stage = DraftStage::Description;
      dialogue.update(ConversationState::AddItem(draft)).await?;
      bot
        .send_message(chat_id, "🧾 Enter description (or '-' to skip):")
        .await?;
    },
    DraftStage::Description => {
      let description = text.map(|value| value.to_string());
      let value = match description.as_deref() {
        Some("-") | None => None,
        _ => description,
      };
      draft.description = value;
      draft.stage = DraftStage::StartPrice;
      dialogue.update(ConversationState::AddItem(draft)).await?;
      bot.send_message(chat_id, "💰 Enter start price (e.g., 50.00):").await?;
    },
    DraftStage::StartPrice => {
      let Some(amount_text) = text else {
        bot
          .send_message(chat_id, "💰 Provide a start price in 0.00 format.")
          .await?;
        return Ok(());
      };
      match parse_money_to_cents(amount_text) {
        Ok(value) => {
          draft.start_price = Some(value);
          let image_ids: Vec<String> = draft.image_file_ids.iter().map(|id| id.to_string()).collect();
          let item_id = ctx
            .db()
            .create_item(
              draft.seller_tg_id,
              draft.category_id.context("missing category during draft completion")?,
              draft
                .title
                .as_deref()
                .context("missing title during draft completion")?,
              draft.description.as_deref(),
              value,
              &image_ids,
            )
            .await?;
          dialogue.reset().await?;
          bot.send_message(chat_id, format!("Item created: #{item_id}")).await?;
          match send_item(&bot, &ctx, chat_id, item_id, Some(draft.seller_tg_id)).await {
            Ok(true) => {},
            Ok(false) => warn!(item_id, "item missing immediately after creation"),
            Err(err) => warn!(error = %err, item_id, "failed to present new item"),
          }
        },
        Err(err) => {
          bot.send_message(chat_id, format!("⚠️ Invalid price: {err}")).await?;
        },
      }
    },
  }

  Ok(())
}

#[instrument(skip(bot, ctx, dialogue, msg))]
async fn handle_bid_message(
  bot: Bot,
  dialogue: BotDialogue,
  ctx: SharedContext,
  msg: Message,
  draft: BidDraft,
) -> HandlerResult {
  let user = msg.from.as_ref().context("message missing sender")?;
  let bidder_id = user.id.0 as i64;
  let chat_id = msg.chat.id;
  info!(bidder_id, chat_id = %chat_id, item_id = draft.item_id, "handling bid input");

  if bidder_id != draft.bidder_tg_id {
    bot.send_message(chat_id, "Another bid is already in progress.").await?;
    return Ok(());
  }

  let Some(amount_text) = message_text(&msg).map(|t| t.trim()).filter(|t| !t.is_empty()) else {
    bot.send_message(chat_id, "Provide your bid in 0.00 format.").await?;
    return Ok(());
  };

  match validate_bid(&ctx, draft.item_id, amount_text).await {
    Ok((item, amount_cents, previous_best)) => match ctx.db().place_bid(draft.item_id, bidder_id, amount_cents).await
    {
      Ok(_) => {
        dialogue.reset().await?;

        let highest = ctx.db().best_bid_with_bidder(draft.item_id).await?;
        let mut confirmation = format!(
          "Bid placed at {} for item #{}.",
          format_cents(amount_cents),
          draft.item_id
        );
        let is_highest = matches!(
          highest,
          Some((top_bidder, top_amount)) if top_bidder == bidder_id && top_amount == amount_cents
        );
        if is_highest {
          confirmation.push_str("\n\n🎉 You're now the highest bidder!");
        }

        bot.send_message(chat_id, confirmation).await?;

        if is_highest
          && let Some((outbid_user_id, outbid_amount)) = previous_best
          && outbid_user_id != bidder_id
          && let Err(err) =
            notify_outbid_user(&bot, &ctx, &item, outbid_user_id, outbid_amount, amount_cents, user).await
        {
          warn!(error = %err, item_id = item.id, outbid_user_id, "failed to notify outbid user");
        }

        let _ = notify_seller(&bot, &ctx, &item, user, amount_cents).await;
        info!(bidder_id, item_id = draft.item_id, amount_cents, "bid accepted");
        match send_item(&bot, &ctx, chat_id, draft.item_id, Some(bidder_id)).await {
          Ok(true) => {},
          Ok(false) => warn!(item_id = draft.item_id, "item no longer available after bid"),
          Err(err) => warn!(error = %err, item_id = draft.item_id, "failed to present item after bid"),
        }
      },
      Err(err) => {
        warn!(error = %err, item_id = draft.item_id, bidder_id, "failed to store bid");
        bot
          .send_message(chat_id, "Failed to place bid, try again later.")
          .await?;
      },
    },
    Err(BidError::Storage(err)) => {
      warn!(error = %err, item_id = draft.item_id, bidder_id, "storage error during bid validation");
      bot
        .send_message(chat_id, "Failed to place bid, try again later.")
        .await?;
    },
    Err(other) => {
      bot.send_message(chat_id, other.user_message()).await?;
      if matches!(other, BidError::NotFound | BidError::Closed) {
        dialogue.reset().await?;
      }
    },
  }

  Ok(())
}

#[instrument(skip(bot, ctx, dialogue, msg))]
async fn handle_add_category_message(
  bot: Bot,
  dialogue: BotDialogue,
  ctx: SharedContext,
  msg: Message,
  admin_tg_id: i64,
) -> HandlerResult {
  let user = msg.from.as_ref().context("message missing sender")?;
  if user.id.0 as i64 != admin_tg_id {
    bot
      .send_message(msg.chat.id, "Only the admin who started this action can respond.")
      .await?;
    return Ok(());
  }

  let Some(raw_text) = message_text(&msg).map(|t| t.trim()).filter(|t| !t.is_empty()) else {
    bot
      .send_message(msg.chat.id, "🆕 Send the new category name or type cancel to stop.")
      .await?;
    return Ok(());
  };

  info!(admin_tg_id, chat_id = %msg.chat.id, "processing add category input");

  if raw_text.eq_ignore_ascii_case("cancel") {
    dialogue.reset().await?;
    bot.send_message(msg.chat.id, "❌ Category creation cancelled.").await?;
    return Ok(());
  }

  let (category, existing) = ensure_category(&ctx, raw_text).await?;
  info!(admin_tg_id, category_id = category.id, existing, "ensured category");
  dialogue.reset().await?;

  let response = if existing {
    format!("⚠️ Category already exists: {} (#{})", category.name, category.id)
  } else {
    format!("✅ Category created: {} (#{})", category.name, category.id)
  };

  bot.send_message(msg.chat.id, response).await?;
  Ok(())
}

fn build_category_picker_keyboard(categories: &[CategoryRow]) -> InlineKeyboardMarkup {
  let mut rows = Vec::new();

  for chunk in categories.chunks(2) {
    rows.push(
      chunk
        .iter()
        .map(|c| InlineKeyboardButton::callback(c.name.clone(), format!("pickcat:{}", c.id)))
        .collect::<Vec<_>>(),
    );
  }

  // footer
  let mut footer = vec![InlineKeyboardButton::callback(
    "➕ New category",
    "pickcat:new".to_string(),
  )];

  footer.push(InlineKeyboardButton::callback("⬅️ Main menu", "menu:root".to_string()));

  rows.push(footer);
  InlineKeyboardMarkup::new(rows)
}

#[instrument(skip(bot, ctx))]
async fn send_category_picker_message(bot: &Bot, ctx: &SharedContext, chat: ChatId) -> HandlerResult {
  let categories = ctx.db().list_categories().await?;
  if categories.is_empty() {
    info!(chat_id = %chat, "no categories to show in picker");
    bot
      .send_message(
        chat,
        "🗂️ No categories yet.\nSend a new category name, or /cancel to stop.",
      )
      .await?;
  } else {
    info!(chat_id = %chat, count = categories.len(), "sending category picker");
    let kb = build_category_picker_keyboard(&categories);
    let txt = format!(
      "🗂️ Choose a category \\(or tap {}\\):",
      teloxide::utils::markdown::bold("➕ New category")
    );
    bot
      .send_message(chat, txt)
      .parse_mode(ParseMode::MarkdownV2)
      .reply_markup(kb)
      .await?;
  }
  Ok(())
}

#[instrument(skip(bot, ctx, dialogue, msg))]
async fn handle_close_item_message(
  bot: Bot,
  dialogue: BotDialogue,
  ctx: SharedContext,
  msg: Message,
  admin_tg_id: i64,
) -> HandlerResult {
  let user = msg.from.as_ref().context("message missing sender")?;
  if user.id.0 as i64 != admin_tg_id {
    bot
      .send_message(msg.chat.id, "Only the admin who started this action can respond.")
      .await?;
    return Ok(());
  }

  let Some(raw_text) = message_text(&msg).map(|t| t.trim()).filter(|t| !t.is_empty()) else {
    bot
      .send_message(msg.chat.id, "🛑 Send the item ID to close or type cancel to stop.")
      .await?;
    return Ok(());
  };

  info!(admin_tg_id, chat_id = %msg.chat.id, "processing close item input");
  if raw_text.eq_ignore_ascii_case("cancel") {
    dialogue.reset().await?;
    bot.send_message(msg.chat.id, "❌ Item closure cancelled.").await?;
    info!(admin_tg_id, chat_id = %msg.chat.id, "close item cancelled by admin");
    return Ok(());
  }

  let item_id: i64 = match raw_text.parse() {
    Ok(value) => value,
    Err(_) => {
      bot.send_message(msg.chat.id, "🔢 Provide a numeric item ID.").await?;
      return Ok(());
    },
  };

  let Some(item) = ctx.db().get_item(item_id).await? else {
    bot.send_message(msg.chat.id, "❓ Item not found.").await?;
    return Ok(());
  };

  if !item.is_open {
    dialogue.reset().await?;
    bot
      .send_message(msg.chat.id, format!("ℹ️ Item #{} is already closed.", item_id))
      .await?;
    info!(admin_tg_id, item_id, "close item requested but already closed");
    return Ok(());
  }

  ctx.db().close_item(item_id).await?;
  info!(admin_tg_id, item_id, "closed item");
  dialogue.reset().await?;
  bot
    .send_message(msg.chat.id, format!("🛑 Item #{item_id} closed."))
    .await?;

  if let Err(err) = notify_item_closed(&bot, &ctx, &item).await {
    warn!(error = %err, item_id, "failed to notify watchers about closed item");
  }
  Ok(())
}

#[instrument(skip(bot, ctx, dialogue, msg))]
async fn handle_broadcast_message(
  bot: Bot,
  dialogue: BotDialogue,
  ctx: SharedContext,
  msg: Message,
  admin_tg_id: i64,
) -> HandlerResult {
  let user = msg.from.as_ref().context("message missing sender")?;
  if user.id.0 as i64 != admin_tg_id {
    bot
      .send_message(msg.chat.id, "Only the admin who started this action can respond.")
      .await?;
    return Ok(());
  }

  let Some(raw_text) = message_text(&msg) else {
    bot
      .send_message(
        msg.chat.id,
        "📢 Send the announcement text (formatting will be preserved).",
      )
      .await?;
    return Ok(());
  };

  let text = raw_text.to_string();
  let entities: Vec<MessageEntity> = msg.entities().map(|slice| slice.to_vec()).unwrap_or_default();

  let recipients = ctx.db().list_user_ids().await?;
  info!(
    admin_tg_id,
    recipient_count = recipients.len(),
    "preparing broadcast message"
  );

  if recipients.is_empty() {
    dialogue.reset().await?;
    bot
      .send_message(msg.chat.id, "📢 No users are registered to receive the announcement.")
      .await?;
    return Ok(());
  }

  let delivered = broadcast_text(&bot, &recipients, &text, (!entities.is_empty()).then_some(&entities)).await;

  dialogue.reset().await?;
  bot
    .send_message(msg.chat.id, format!("📢 Broadcast sent to {delivered} user(s)."))
    .await?;
  Ok(())
}

#[instrument(skip(bot, ctx, dialogue, msg))]
async fn handle_remove_item_message(
  bot: Bot,
  dialogue: BotDialogue,
  ctx: SharedContext,
  msg: Message,
  admin_tg_id: i64,
) -> HandlerResult {
  let user = msg.from.as_ref().context("message missing sender")?;
  if user.id.0 as i64 != admin_tg_id {
    bot
      .send_message(msg.chat.id, "Only the admin who started this action can respond.")
      .await?;
    return Ok(());
  }

  let Some(raw_text) = message_text(&msg).map(|t| t.trim()).filter(|t| !t.is_empty()) else {
    bot
      .send_message(msg.chat.id, "🗑 Send the item ID to remove or type cancel to stop.")
      .await?;
    return Ok(());
  };

  info!(admin_tg_id, chat_id = %msg.chat.id, "processing remove item input");
  if raw_text.eq_ignore_ascii_case("cancel") {
    dialogue.reset().await?;
    bot.send_message(msg.chat.id, "❌ Item removal cancelled.").await?;
    info!(admin_tg_id, chat_id = %msg.chat.id, "remove item cancelled by admin");
    return Ok(());
  }

  let item_id: i64 = match raw_text.parse() {
    Ok(value) => value,
    Err(_) => {
      bot.send_message(msg.chat.id, "🔢 Provide a numeric item ID.").await?;
      return Ok(());
    },
  };

  if ctx.db().delete_item(item_id).await? {
    dialogue.reset().await?;
    info!(admin_tg_id, item_id, "item removed");
    bot
      .send_message(
        msg.chat.id,
        format!("🗑 Item #{item_id} removed. Related bids and favorites were deleted."),
      )
      .await?;
  } else {
    info!(admin_tg_id, item_id, "item not found for removal");
    bot
      .send_message(msg.chat.id, "❓ Item not found or already removed.")
      .await?;
  }

  Ok(())
}

#[instrument(skip(bot, ctx, dialogue, msg))]
async fn handle_remove_category_message(
  bot: Bot,
  dialogue: BotDialogue,
  ctx: SharedContext,
  msg: Message,
  admin_tg_id: i64,
) -> HandlerResult {
  let user = msg.from.as_ref().context("message missing sender")?;
  if user.id.0 as i64 != admin_tg_id {
    bot
      .send_message(msg.chat.id, "Only the admin who started this action can respond.")
      .await?;
    return Ok(());
  }

  let Some(raw_text) = message_text(&msg).map(|t| t.trim()).filter(|t| !t.is_empty()) else {
    bot
      .send_message(
        msg.chat.id,
        "🗑 Send the category name to remove or type cancel to stop.",
      )
      .await?;
    return Ok(());
  };

  info!(admin_tg_id, chat_id = %msg.chat.id, "processing remove category input");
  if raw_text.eq_ignore_ascii_case("cancel") {
    dialogue.reset().await?;
    bot.send_message(msg.chat.id, "❌ Category removal cancelled.").await?;
    info!(admin_tg_id, chat_id = %msg.chat.id, "remove category cancelled by admin");
    return Ok(());
  }

  let Some(category) = ctx.db().find_category_by_name(raw_text).await? else {
    bot.send_message(msg.chat.id, "❓ Category not found.").await?;
    return Ok(());
  };

  info!(admin_tg_id, category_id = category.id, "category found for removal");

  let item_count = ctx.db().list_items_by_category(category.id).await?.len();
  info!(admin_tg_id, category_id = category.id, item_count, "removing category");
  if ctx.db().delete_category(category.id).await? {
    dialogue.reset().await?;
    info!(admin_tg_id, category_id = category.id, item_count, "category removed");
    bot
      .send_message(
        msg.chat.id,
        format!(
          "🗑 Category '{}' removed. {} item(s) and related bids/favorites were deleted.",
          category.name, item_count
        ),
      )
      .await?;
  } else {
    info!(admin_tg_id, category_id = category.id, "category not removed");
    bot
      .send_message(msg.chat.id, "❓ Category not found or already removed.")
      .await?;
  }

  Ok(())
}

#[instrument(skip(bot, msg))]
async fn handle_idle_text(bot: Bot, msg: Message, state: ConversationState) -> HandlerResult {
  if matches!(state, ConversationState::Idle)
    && let Some(text) = msg.text()
  {
    if text.starts_with('/') {
      // unknown command, ignore to let telegram handle
    } else {
      info!(chat_id = %msg.chat.id, "idle state received unrecognized message");
      bot
        .send_message(msg.chat.id, "I did not understand that. Use the menu buttons or /help.")
        .await?;
    }
  }
  Ok(())
}

#[instrument(skip(bot, ctx, dialogue, query))]
async fn handle_callback_query(
  bot: Bot,
  ctx: SharedContext,
  query: CallbackQuery,
  dialogue: BotDialogue,
) -> HandlerResult {
  ensure_user_record(&ctx, &query.from).await?;
  let mut callback_text: Option<String> = None;
  let user_id = query.from.id.0 as i64;
  let message_ctx = query.message.as_ref().map(|message| (message.chat().id, message.id()));
  let callback_data = query.data.as_deref().unwrap_or("<empty>");
  if let Some((chat_id, _)) = message_ctx {
    info!(user_id, chat_id = %chat_id, callback = callback_data, "handling callback query");
  } else {
    info!(
      user_id,
      callback = callback_data,
      "handling callback query without message context"
    );
  }

  if let Some(data) = query.data.as_deref()
    && let Some((prefix, value)) = data.split_once(':')
  {
    match prefix {
      "menu" => match value {
        "root" => {
          dialogue.reset().await?;
          if let Some((chat_id, message_id)) = message_ctx {
            show_main_menu(&bot, &ctx, chat_id, message_id, user_id).await?;
          }
        },
        "catalogue" => {
          dialogue.reset().await?;
          if let Some((chat_id, message_id)) = message_ctx {
            show_catalogue_menu(&bot, &ctx, chat_id, message_id).await?;
          }
        },
        "favorites" => {
          if let Some((chat_id, _)) = message_ctx {
            send_favorites_list(&bot, &ctx, chat_id, user_id).await?;
            callback_text = Some("⭐ Sent your favorites.".to_string());
          }
        },
        "my_bids" => {
          if let Some((chat_id, _)) = message_ctx {
            send_my_bids_list(&bot, &ctx, chat_id, user_id).await?;
            callback_text = Some("🪙 Sent your bids.".to_string());
          }
        },
        "settings" => {
          dialogue.reset().await?;
          if let Some((chat_id, message_id)) = message_ctx {
            show_settings_menu(&bot, &ctx, chat_id, message_id, user_id).await?;
          }
        },
        "admin" => {
          if ctx.is_admin(user_id) {
            dialogue.reset().await?;
            if let Some((chat_id, message_id)) = message_ctx {
              show_admin_menu(&bot, chat_id, message_id).await?;
            }
          } else {
            callback_text = Some("🛡️ Admins only.".to_string());
          }
        },
        _ => {},
      },
      "admin" => {
        if !ctx.is_admin(user_id) {
          callback_text = Some("🛡️ Admins only.".to_string());
        } else {
          match value {
            "add_category" => {
              dialogue.reset().await?;
              dialogue
                .update(ConversationState::AddCategory { admin_tg_id: user_id })
                .await?;
              if let Some((chat_id, _)) = message_ctx {
                bot.send_message(chat_id, "🆕 Send the new category name:").await?;
              }
              callback_text = Some("🆕 Waiting for category name.".to_string());
            },
            "add_item" => {
              dialogue.reset().await?;
              dialogue
                .update(ConversationState::AddItem(AddItemDraft::new(user_id, None)))
                .await?;
              if let Some((chat_id, _)) = message_ctx {
                // show picker
                send_category_picker_message(&bot, &ctx, chat_id).await?;
              }
              callback_text = Some("📦 Starting item creation.".to_string());
            },
            "remove_item" => {
              dialogue.reset().await?;
              dialogue
                .update(ConversationState::RemoveItem { admin_tg_id: user_id })
                .await?;
              if let Some((chat_id, _)) = message_ctx {
                bot
                  .send_message(
                    chat_id,
                    "🗑 Send the item ID to remove (bids and favorites will also be removed). Type cancel to stop.",
                  )
                  .await?;
              }
              callback_text = Some("🗑 Awaiting item ID to remove.".to_string());
            },
            "remove_category" => {
              dialogue.reset().await?;
              dialogue
                .update(ConversationState::RemoveCategory { admin_tg_id: user_id })
                .await?;
              if let Some((chat_id, _)) = message_ctx {
                bot
                  .send_message(
                    chat_id,
                    "🗑 Send the category ID to remove (all items, bids, and favorites under it will be deleted). \
                     Type cancel to stop.",
                  )
                  .await?;
              }
              callback_text = Some("🗑 Awaiting category ID to remove.".to_string());
            },
            "close_item" => {
              dialogue.reset().await?;
              dialogue
                .update(ConversationState::CloseItem { admin_tg_id: user_id })
                .await?;
              if let Some((chat_id, _)) = message_ctx {
                bot.send_message(chat_id, "🛑 Send the item ID to close:").await?;
              }
              callback_text = Some("🛑 Awaiting item ID.".to_string());
            },
            "broadcast" => {
              dialogue.reset().await?;
              dialogue
                .update(ConversationState::Broadcast { admin_tg_id: user_id })
                .await?;
              if let Some((chat_id, _)) = message_ctx {
                bot
                  .send_message(chat_id, "📢 Send the announcement text to broadcast to all users.")
                  .await?;
              }
              callback_text = Some("📢 Waiting for announcement text.".to_string());
            },
            "notify_new" => {
              dialogue.reset().await?;
              let new_items = ctx.db().list_new_items().await?;
              if new_items.is_empty() {
                if let Some((chat_id, _)) = message_ctx {
                  bot
                    .send_message(chat_id, "🔔 No new lots are marked for notification.")
                    .await?;
                }
                callback_text = Some("🔔 No new lots.".to_string());
              } else {
                let user_ids = ctx.db().list_user_ids().await?;
                if user_ids.is_empty() {
                  if let Some((chat_id, _)) = message_ctx {
                    bot
                      .send_message(chat_id, "🔔 No users are registered to receive the update.")
                      .await?;
                  }
                  callback_text = Some("🔔 No users registered.".to_string());
                } else {
                  let mut announcement = String::from("🆕 New lots available!\n\n");
                  for item in &new_items {
                    let line = format!(
                      "• #{} {} — start {}\n",
                      item.id,
                      item.title,
                      format_cents(item.start_price)
                    );
                    announcement.push_str(&line);
                  }

                  info!(
                    admin_tg_id = user_id,
                    lot_count = new_items.len(),
                    recipient_count = user_ids.len(),
                    "broadcasting new lots"
                  );
                  let delivered = broadcast_text(&bot, &user_ids, &announcement, None).await;
                  let ids: Vec<i64> = new_items.iter().map(|item| item.id).collect();
                  ctx.db().clear_new_item_flags(&ids).await?;

                  if let Some((chat_id, _)) = message_ctx {
                    bot
                      .send_message(
                        chat_id,
                        format!("🔔 Notified {delivered} user(s) about {} new lot(s).", new_items.len()),
                      )
                      .await?;
                  }
                  callback_text = Some("🔔 Notification sent.".to_string());
                }
              }
            },
            _ => {},
          }
        }
      },
      "pickcat" => {
        if let Some((chat_id, _message_id)) = message_ctx {
          match value {
            "new" => {
              let state = dialogue.get().await?;
              if !matches!(state, Some(ConversationState::AddItem(_))) {
                dialogue
                  .update(ConversationState::AddItem(AddItemDraft::new(user_id, None)))
                  .await?;
              }
              bot
                .send_message(chat_id, "🆕 Send the new category name (or type cancel).")
                .await?;
              callback_text = Some("🆕 Waiting for category name.".to_string());
            },
            id_str => {
              if let Ok(category_id) = id_str.parse::<i64>() {
                let categories = ctx.db().list_categories().await?;
                if let Some(category) = categories.into_iter().find(|c| c.id == category_id) {
                  if let Some(ConversationState::AddItem(mut draft)) = dialogue.get().await? {
                    draft.category_id = Some(category.id);
                    draft.category_name = Some(category.name);
                    draft.stage = DraftStage::Title;
                    dialogue.update(ConversationState::AddItem(draft)).await?;
                    bot.send_message(chat_id, "📝 Enter item title:").await?;
                    callback_text = Some("🗂️ Category selected.".to_string());
                  } else {
                    let mut draft = AddItemDraft::new(user_id, None);
                    draft.category_id = Some(category.id);
                    draft.category_name = Some(category.name);
                    draft.stage = DraftStage::Title;
                    dialogue.update(ConversationState::AddItem(draft)).await?;
                    bot.send_message(chat_id, "📝 Enter item title:").await?;
                    callback_text = Some("🗂️ Category selected.".to_string());
                  }
                } else {
                  callback_text = Some("❓ Category not found".to_string());
                }
              }
            },
          }
        }
      },
      "cat" => {
        if let Ok(category_id) = value.parse::<i64>()
          && let Some((chat_id, message_id)) = message_ctx
        {
          let categories = ctx.db().list_categories().await?;
          if let Some(category) = categories.into_iter().find(|c| c.id == category_id) {
            show_category_items_menu(&bot, &ctx, chat_id, message_id, category.id, category.name.as_str()).await?;
          } else {
            callback_text = Some("❓ Category not found".to_string());
          }
        }
      },
      "item" => {
        if let Ok(item_id) = value.parse::<i64>()
          && let Some((chat_id, _)) = message_ctx
          && !send_item(&bot, &ctx, chat_id, item_id, Some(user_id)).await?
        {
          callback_text = Some("❓ Item not found".to_string());
        }
      },
      "img" => {
        let mut parts = value.split(':');
        if let (Some(item_str), Some(offset_str)) = (parts.next(), parts.next())
          && let (Ok(item_id), Ok(offset)) = (item_str.parse::<i64>(), offset_str.parse::<usize>())
        {
          let images = ctx.db().list_item_images(item_id).await?;
          if let Some((chat_id, message_id)) = message_ctx {
            let total = images.len();
            if offset >= total {
              let request = bot
                .edit_message_text(chat_id, message_id, "📷 All images shown.")
                .reply_markup(InlineKeyboardMarkup::default());
              if let Err(err) = request.await
                && !matches!(err, RequestError::Api(ApiError::MessageNotModified))
              {
                return Err(err.into());
              }
              callback_text = Some("📷 All images already shown.".to_string());
            } else {
              let next = send_item_images_chunk(&bot, chat_id, &images, offset, None).await?;
              if next < total {
                let remaining = total - next;
                let keyboard = InlineKeyboardMarkup::new(vec![vec![InlineKeyboardButton::callback(
                  format!("Show more images ({remaining})"),
                  format!("img:{item_id}:{next}"),
                )]]);
                let request = bot
                  .edit_message_text(chat_id, message_id, format!("📷 {remaining} more photo(s) available."))
                  .reply_markup(keyboard);
                if let Err(err) = request.await
                  && !matches!(err, RequestError::Api(ApiError::MessageNotModified))
                {
                  return Err(err.into());
                }
              } else {
                let request = bot
                  .edit_message_text(chat_id, message_id, "📷 All images shown.")
                  .reply_markup(InlineKeyboardMarkup::default());
                if let Err(err) = request.await
                  && !matches!(err, RequestError::Api(ApiError::MessageNotModified))
                {
                  return Err(err.into());
                }
              }
              callback_text = Some("📷 Sent more photos.".to_string());
            }
          }
        }
      },
      "back" => {
        if value == "categories"
          && let Some((chat_id, message_id)) = message_ctx
        {
          show_catalogue_menu(&bot, &ctx, chat_id, message_id).await?;
        }
      },
      "bid" => {
        if let Ok(item_id) = value.parse::<i64>() {
          match ctx.db().get_item(item_id).await? {
            Some(item) if item.is_open => {
              dialogue
                .update(ConversationState::PlaceBid(BidDraft {
                  item_id,
                  bidder_tg_id: user_id,
                }))
                .await?;
              if let Some((chat_id, _)) = message_ctx {
                bot
                  .send_message(chat_id, format!("Enter your bid for item #{item_id} in 0.00 format:"))
                  .await?;
              }
            },
            Some(_) => {
              callback_text = Some("🔒 Auction is closed".to_string());
            },
            None => {
              callback_text = Some("❓ Item not found".to_string());
            },
          }
        }
      },
      "fav" => {
        if let Some((action, item_str)) = value.split_once(':')
          && let Ok(item_id) = item_str.parse::<i64>()
        {
          match action {
            "add" => {
              ctx.db().add_favorite(user_id, item_id).await?;
              callback_text = Some("⭐ Added to favorites".to_string());
            },
            "remove" => {
              ctx.db().remove_favorite(user_id, item_id).await?;
              callback_text = Some("❌ Removed from favorites".to_string());
            },
            _ => {},
          }

          if let Some((chat_id, message_id)) = message_ctx
            && let Some(item) = ctx.db().get_item(item_id).await?
          {
            let viewer = build_item_viewer_context(&ctx, item_id, user_id).await?;
            let keyboard = item_action_keyboard(item.id, item.is_open, Some(&viewer));
            if let Err(err) = bot
              .edit_message_reply_markup(chat_id, message_id)
              .reply_markup(keyboard)
              .await
              && !matches!(err, RequestError::Api(ApiError::MessageNotModified))
            {
              return Err(err.into());
            }
          }
        }
      },
      "settings" => match value {
        "toggle_notifications" => {
          let currently_disabled = ctx.db().notifications_disabled(user_id).await?;
          let next = !currently_disabled;
          ctx.db().set_notifications_disabled(user_id, next).await?;
          if let Some((chat_id, message_id)) = message_ctx {
            show_settings_menu(&bot, &ctx, chat_id, message_id, user_id).await?;
          }
          callback_text = Some(if next {
            "🔕 Notifications muted.".to_string()
          } else {
            "🔔 Notifications enabled.".to_string()
          });
        },
        _ => {},
      },
      _ => {},
    }
  }

  if let Some(text) = callback_text {
    bot.answer_callback_query(query.id).text(text).await?;
  } else {
    bot.answer_callback_query(query.id).await?;
  }
  Ok(())
}

async fn update_categories_menu(
  bot: &Bot,
  ctx: &SharedContext,
  chat: ChatId,
  message_id: MessageId,
) -> HandlerResult {
  let categories = ctx.db().list_categories().await?;
  if categories.is_empty() {
    let request = bot
      .edit_message_text(chat, message_id, "🗂️ No categories yet. Check back soon.")
      .reply_markup(main_menu_only_keyboard());
    match request.await {
      Ok(_) => info!(chat_id = %chat, message_id = %message_id, "rendered empty categories menu"),
      Err(RequestError::Api(ApiError::MessageNotModified)) => {
        info!(chat_id = %chat, message_id = %message_id, "categories menu already empty");
        return Ok(());
      },
      Err(err) => return Err(err.into()),
    }
  } else {
    let keyboard = build_categories_keyboard(&categories);
    let request = bot
      .edit_message_text(chat, message_id, "🗂️ Choose a category:")
      .reply_markup(keyboard);
    match request.await {
      Ok(_) => info!(chat_id = %chat, message_id = %message_id, count = categories.len(), "rendered categories menu"),
      Err(RequestError::Api(ApiError::MessageNotModified)) => {
        info!(chat_id = %chat, message_id = %message_id, "categories menu already current");
        return Ok(());
      },
      Err(err) => return Err(err.into()),
    }
  }
  Ok(())
}

async fn show_category_items_menu(
  bot: &Bot,
  ctx: &SharedContext,
  chat: ChatId,
  message_id: MessageId,
  category_id: i64,
  category_name: &str,
) -> HandlerResult {
  let items = ctx.db().list_items_by_category(category_id).await?;
  info!(category_id, count = items.len(), chat_id = %chat, "rendering category items menu");
  let text = if items.is_empty() {
    format!("🗂️ Category: {category_name}\n📭 No items in this category yet.")
  } else {
    format!("🗂️ Category: {category_name}\n🛍️ Select an item:")
  };
  let keyboard = build_items_keyboard(ctx, &items).await;
  let request = bot.edit_message_text(chat, message_id, text).reply_markup(keyboard);
  match request.await {
    Ok(_) => info!(category_id, chat_id = %chat, message_id = %message_id, "rendered category items menu"),
    Err(RequestError::Api(ApiError::MessageNotModified)) => {
      info!(category_id, chat_id = %chat, message_id = %message_id, "category items menu already current");
      return Ok(());
    },
    Err(err) => return Err(err.into()),
  }
  Ok(())
}

fn build_categories_keyboard(categories: &[CategoryRow]) -> InlineKeyboardMarkup {
  let mut rows = categories
    .chunks(2)
    .map(|row| {
      row
        .iter()
        .map(|category| InlineKeyboardButton::callback(category.name.clone(), format!("cat:{}", category.id)))
        .collect::<Vec<_>>()
    })
    .collect::<Vec<_>>();

  rows.push(vec![InlineKeyboardButton::callback(
    "⬅️ Main menu",
    "menu:root".to_string(),
  )]);

  InlineKeyboardMarkup::new(rows)
}

async fn build_items_keyboard(ctx: &SharedContext, items: &[ItemRow]) -> InlineKeyboardMarkup {
  use futures::future::join_all;

  let bids = join_all(items.iter().map(|it| ctx.db().best_bid_for_item(it.id))).await;

  let mut enriched: Vec<(&ItemRow, Option<i64>)> =
    items.iter().zip(bids.into_iter().map(|r| r.unwrap_or(None))).collect();

  enriched.sort_by_key(|(it, _best)| !it.is_open);

  let mut rows: Vec<Vec<InlineKeyboardButton>> = Vec::new();
  for (item, best) in enriched {
    let price_cents = best.unwrap_or(item.start_price);
    let mut label = format!(
      "{}{} — {}",
      if item.is_open { "" } else { "🔴 " },
      format_cents(price_cents),
      &item.title
    );
    label = truncate_button_text(&label, 48);

    rows.push(vec![InlineKeyboardButton::callback(label, format!("item:{}", item.id))]);
  }

  rows.push(vec![
    InlineKeyboardButton::callback("⬅️ Categories".to_string(), "back:categories".to_string()),
    InlineKeyboardButton::callback("⬅️ Main menu".to_string(), "menu:root".to_string()),
  ]);

  InlineKeyboardMarkup::new(rows)
}

fn truncate_button_text(text: &str, max_chars: usize) -> String {
  if text.chars().count() <= max_chars {
    return text.to_string();
  }

  let guarded = max_chars.saturating_sub(3);
  if guarded == 0 {
    return "...".to_string();
  }

  let truncated: String = text.chars().take(guarded).collect();
  format!("{truncated}...")
}

struct ItemViewerContext {
  is_favorite: bool,
  user_best_bid: Option<i64>,
}

async fn build_item_viewer_context(ctx: &SharedContext, item_id: i64, user_id: i64) -> Result<ItemViewerContext> {
  let is_favorite = ctx.db().is_favorite(user_id, item_id).await?;
  let user_best_bid = ctx.db().user_best_bid_for_item(item_id, user_id).await?;
  Ok(ItemViewerContext {
    is_favorite,
    user_best_bid,
  })
}

async fn send_item(
  bot: &Bot,
  ctx: &SharedContext,
  chat: ChatId,
  item_id: i64,
  viewer_id: Option<i64>,
) -> Result<bool> {
  let Some(item) = ctx.db().get_item(item_id).await? else {
    return Ok(false);
  };
  let best = ctx.db().best_bid_for_item(item_id).await?;
  let viewer_ctx = match viewer_id {
    Some(user_id) => Some(build_item_viewer_context(ctx, item_id, user_id).await?),
    None => None,
  };
  let text = render_item_message(&item, best, viewer_ctx.as_ref());
  let keyboard = item_action_keyboard(item.id, item.is_open, viewer_ctx.as_ref());

  bot
    .send_message(chat, text.clone())
    .parse_mode(ParseMode::MarkdownV2)
    .reply_markup(keyboard)
    .await?;

  let mut images = ctx.db().list_item_images(item.id).await?;
  if images.is_empty()
    && let Some(legacy_cover) = item.image_file_id.clone()
  {
    images.push(legacy_cover);
  }

  if !images.is_empty() {
    let next_offset = send_item_images_chunk(bot, chat, &images, 0, None).await?;
    if next_offset < images.len() {
      send_more_images_prompt(bot, chat, item.id, next_offset, images.len()).await?;
    }
  }

  Ok(true)
}

async fn send_item_images_chunk(
  bot: &Bot,
  chat: ChatId,
  images: &[FileId],
  start: usize,
  caption: Option<&str>,
) -> Result<usize> {
  if start >= images.len() {
    return Ok(start);
  }

  let end = (start + MEDIA_GROUP_BATCH).min(images.len());
  let mut media = Vec::new();
  for (index, file_id) in images[start .. end].iter().enumerate() {
    let mut photo = InputMediaPhoto::new(InputFile::file_id(file_id.clone()));
    if let Some(text) = caption
      && start == 0
      && index == 0
    {
      photo = photo.caption(text.to_string()).parse_mode(ParseMode::MarkdownV2);
    }
    media.push(InputMedia::Photo(photo));
  }

  bot.send_media_group(chat, media).await?;
  Ok(end)
}

async fn send_more_images_prompt(
  bot: &Bot,
  chat: ChatId,
  item_id: i64,
  next_offset: usize,
  total: usize,
) -> HandlerResult {
  let remaining = total.saturating_sub(next_offset);
  let text = format!("📷 {remaining} more photo(s) available.");
  let keyboard = InlineKeyboardMarkup::new(vec![vec![InlineKeyboardButton::callback(
    format!("Show more images ({remaining})"),
    format!("img:{item_id}:{next_offset}"),
  )]]);
  bot.send_message(chat, text).reply_markup(keyboard).await?;
  Ok(())
}

fn render_item_message(item: &ItemRow, best: Option<i64>, viewer: Option<&ItemViewerContext>) -> String {
  let escaped_id = markdown::escape(&format!("#{}", item.id));
  let escaped_title = markdown::escape(&item.title);
  let escaped_start = markdown::escape(&format_cents(item.start_price));

  let mut text = format!("🔨 *{}* — *{}*", escaped_id, escaped_title);

  if let Some(description) = item.description.as_deref()
    && !description.trim().is_empty()
  {
    let escaped_description = markdown::escape(description);
    text.push_str(&format!("\n\n{}", escaped_description));
  }

  text.push_str(&format!("\n\n💰 Start: {}", escaped_start));

  if let Some(best_bid) = best {
    let escaped_best = markdown::escape(&format_cents(best_bid));
    text.push_str(&format!("\n🏆 Current best: {}", escaped_best));
  }

  if let Some(viewer_ctx) = viewer {
    if let Some(user_bid) = viewer_ctx.user_best_bid {
      let line = markdown::escape(&format!("🎯 Your top bid: {}", format_cents(user_bid)));
      text.push_str(&format!("\n{}", line));
    }
    if viewer_ctx.is_favorite {
      let line = markdown::escape("⭐ Saved to favorites");
      text.push_str(&format!("\n{}", line));
    }
  }

  if item.is_new {
    let line = markdown::escape("🆕 Newly listed");
    text.push_str(&format!("\n{}", line));
  }

  text.push_str(&format!(
    "\n📦 Status: {}",
    if item.is_open { "OPEN" } else { "CLOSED" }
  ));
  text
}

fn item_action_keyboard(item_id: i64, open: bool, viewer: Option<&ItemViewerContext>) -> InlineKeyboardMarkup {
  let mut row = Vec::new();
  if open {
    row.push(InlineKeyboardButton::callback("💸 Place bid", format!("bid:{item_id}")));
  }

  if let Some(viewer_ctx) = viewer {
    let (label, action) = if viewer_ctx.is_favorite {
      ("❌ Remove favorite", "fav:remove")
    } else {
      ("⭐ Add favorite", "fav:add")
    };
    row.push(InlineKeyboardButton::callback(
      label.to_string(),
      format!("{action}:{item_id}"),
    ));
  }

  if row.is_empty() {
    InlineKeyboardMarkup::default()
  } else {
    InlineKeyboardMarkup::new(vec![row])
  }
}

async fn broadcast_text(bot: &Bot, user_ids: &[i64], text: &str, entities: Option<&[MessageEntity]>) -> usize {
  let mut delivered = 0usize;
  let payload = text.to_string();
  let entity_payload = entities.map(|data| data.to_vec());
  for user_id in user_ids {
    let mut request = bot.send_message(ChatId(*user_id), payload.clone());
    if let Some(entities) = &entity_payload {
      request = request.entities(entities.clone());
    }
    match request.await {
      Ok(_) => {
        delivered += 1;
      },
      Err(err) => {
        warn!(error = %err, target_user_id = user_id, "failed to deliver broadcast");
      },
    }
  }
  delivered
}

async fn notify_outbid_user(
  bot: &Bot,
  ctx: &SharedContext,
  item: &ItemRow,
  previous_bidder_id: i64,
  previous_amount_cents: i64,
  new_amount_cents: i64,
  new_bidder: &User,
) -> Result<()> {
  if ctx.db().notifications_disabled(previous_bidder_id).await? {
    return Ok(());
  }

  let bidder_label = if let Some(username) = &new_bidder.username {
    format!("@{username}")
  } else if let Some(last) = &new_bidder.last_name {
    format!("{} {last}", new_bidder.first_name)
  } else {
    new_bidder.first_name.clone()
  };

  let message = format!(
    "⚠️ Your bid of {} on item #{} ({}) was beaten by {}. New highest bid: {}.",
    format_cents(previous_amount_cents),
    item.id,
    item.title,
    bidder_label,
    format_cents(new_amount_cents),
  );

  bot.send_message(ChatId(previous_bidder_id), message).await?;
  Ok(())
}

async fn notify_item_closed(bot: &Bot, ctx: &SharedContext, item: &ItemRow) -> Result<()> {
  let db = ctx.db();
  let winning_bid = db.best_bid_with_bidder(item.id).await?;
  let bidder_ids = db.list_item_bidder_ids(item.id).await?;
  let favorite_ids = db.list_item_favorite_user_ids(item.id).await?;

  let mut recipients: HashSet<i64> = HashSet::new();
  recipients.extend(bidder_ids);
  recipients.extend(favorite_ids);

  let recipients: Vec<i64> = recipients.into_iter().collect();
  if recipients.is_empty() {
    return Ok(());
  }

  let recipients = ctx.db().filter_notifications_allowed(&recipients).await?;
  if recipients.is_empty() {
    return Ok(());
  }

  for user_id in recipients {
    let text = match winning_bid {
      Some((winner_id, amount)) if user_id == winner_id => format!(
        "🏁 Auction closed for item #{} ({}).\n\n🎉 Congratulations! You won with a bid of {}.",
        item.id,
        item.title,
        format_cents(amount),
      ),
      Some((_, amount)) => format!(
        "🏁 Auction closed for item #{} ({}).\nFinal price: {}. Thanks for taking part!",
        item.id,
        item.title,
        format_cents(amount),
      ),
      None => format!(
        "🏁 Auction closed for item #{} ({}).\nThe item closed with no bids.",
        item.id, item.title,
      ),
    };

    if let Err(err) = bot.send_message(ChatId(user_id), text).await {
      warn!(error = %err, item_id = item.id, user_id, "failed to notify user about item closure");
    }
  }

  Ok(())
}

async fn ensure_user_record(ctx: &SharedContext, user: &User) -> Result<()> {
  ctx
    .db()
    .upsert_user(
      user.id.0 as i64,
      user.username.clone(),
      Some(user.first_name.clone()),
      user.last_name.clone(),
    )
    .await
    .context("failed to upsert user record")
}

async fn notify_seller(bot: &Bot, ctx: &SharedContext, item: &ItemRow, user: &User, amount_cents: i64) -> Result<()> {
  if ctx.db().notifications_disabled(item.seller_tg_id).await? {
    return Ok(());
  }

  let username = user.username.clone().unwrap_or_else(|| user.id.0.to_string());
  bot
    .send_message(
      ChatId(item.seller_tg_id),
      format!(
        "New bid on item #{} ({}): @{} offered {}",
        item.id,
        item.title,
        username,
        format_cents(amount_cents),
      ),
    )
    .await?;
  Ok(())
}

async fn ensure_category(ctx: &SharedContext, name: &str) -> Result<(CategoryRow, bool)> {
  if let Some(existing) = ctx.db().find_category_by_name(name).await? {
    return Ok((existing, true));
  }
  let id = ctx.db().create_category(name).await?;
  Ok((
    CategoryRow {
      id,
      name: name.to_string(),
    },
    false,
  ))
}

fn message_text(msg: &Message) -> Option<&str> {
  msg.text().or_else(|| msg.caption())
}

#[derive(Debug, Error)]
enum BidError {
  #[error(transparent)]
  Storage(#[from] SqlxError),
  #[error(transparent)]
  InvalidAmount(#[from] MoneyError),
  #[error(transparent)]
  Anyhow(#[from] anyhow::Error),
  #[error("item not found")]
  NotFound,
  #[error("auction is closed")]
  Closed,
  #[error("bid must exceed {0}")]
  TooLow(i64),
  #[error("bid must be at least {0}")]
  BelowStart(i64),
}

impl BidError {
  fn user_message(&self) -> String {
    match self {
      Self::InvalidAmount(_) => "Amount must match 0.00 format".to_string(),
      Self::NotFound => "Item not found.".to_string(),
      Self::Closed => "Auction is closed.".to_string(),
      Self::TooLow(value) => format!("Your bid must exceed {}.", format_cents(*value)),
      Self::BelowStart(value) => format!("Your bid must be at least {}.", format_cents(*value)),
      Self::Storage(_) => "Temporary error placing bid.".to_string(),
      Self::Anyhow(e) => format!("Unhandled error: {e:?}").to_string(),
    }
  }
}

async fn validate_bid(
  ctx: &SharedContext,
  item_id: i64,
  amount: &str,
) -> Result<(ItemRow, i64, Option<(i64, i64)>), BidError> {
  let amount_cents = parse_money_to_cents(amount)?;
  let item = ctx.db().get_item(item_id).await?.ok_or(BidError::NotFound)?;
  if !item.is_open {
    return Err(BidError::Closed);
  }

  let previous_best = ctx.db().best_bid_with_bidder(item_id).await?;
  if let Some((_, best_amount)) = previous_best {
    if amount_cents <= best_amount {
      return Err(BidError::TooLow(best_amount));
    }
  } else if amount_cents < item.start_price {
    return Err(BidError::BelowStart(item.start_price));
  }
  Ok((item, amount_cents, previous_best))
}

#[cfg(test)]
mod tests {
  use super::ItemViewerContext;
  use super::item_action_keyboard;
  use super::render_item_message;
  use crate::models::ItemRow;
  use chrono::Utc;

  #[test]
  fn renders_keyboard_only_for_open_items() {
    let keyboard = item_action_keyboard(1, true, None);
    assert!(!keyboard.inline_keyboard.is_empty());

    let closed = item_action_keyboard(2, false, None);
    assert!(closed.inline_keyboard.is_empty());
  }

  #[test]
  fn renders_item_text() {
    let item = ItemRow {
      id: 1,
      seller_tg_id: 1,
      category_id: 1,
      title: "Test".to_string(),
      description: Some("Description".to_string()),
      start_price: 100,
      image_file_id: None,
      is_open: true,
      is_new: false,
      created_at: Utc::now(),
    };
    let text = render_item_message(&item, Some(150), None);
    assert!(text.contains("#1"));
    assert!(text.contains("Current best"));
  }

  #[test]
  fn renders_viewer_details() {
    let item = ItemRow {
      id: 5,
      seller_tg_id: 1,
      category_id: 1,
      title: "Test".to_string(),
      description: None,
      start_price: 100,
      image_file_id: None,
      is_open: true,
      is_new: false,
      created_at: Utc::now(),
    };
    let ctx = ItemViewerContext {
      is_favorite: true,
      user_best_bid: Some(125),
    };
    let text = render_item_message(&item, Some(150), Some(&ctx));
    assert!(text.contains("Your top bid"));
    assert!(text.contains("Saved to favorites"));
  }
}

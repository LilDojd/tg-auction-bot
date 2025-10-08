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
use teloxide::types::InlineKeyboardButton;
use teloxide::types::InlineKeyboardMarkup;
use teloxide::types::InputFile;
use teloxide::types::Message;
use teloxide::types::MessageId;
use teloxide::types::ParseMode;
use teloxide::types::User;
use teloxide::utils::command::BotCommands;
use teloxide::utils::markdown;
use thiserror::Error;
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

pub fn build_schema() -> UpdateHandler<anyhow::Error> {
  let message_handler = Update::filter_message()
    .enter_dialogue::<Message, DialogueStorage, ConversationState>()
    .branch(command_branch())
    .branch(dptree::case![ConversationState::AddItem(draft)].endpoint(handle_additem_message))
    .branch(dptree::case![ConversationState::PlaceBid(draft)].endpoint(handle_bid_message))
    .branch(dptree::endpoint(handle_idle_text));

  let callback_handler = Update::filter_callback_query()
    .enter_dialogue::<CallbackQuery, DialogueStorage, ConversationState>()
    .endpoint(handle_callback_query);

  dptree::entry().branch(message_handler).branch(callback_handler)
}

fn command_branch() -> UpdateHandler<anyhow::Error> {
  dptree::entry()
    .filter_command::<Command>()
    .branch(dptree::case![Command::Start].endpoint(handle_help))
    .branch(dptree::case![Command::Help].endpoint(handle_help))
    .branch(dptree::case![Command::Browse].endpoint(handle_browse))
    .branch(dptree::case![Command::Item { id }].endpoint(handle_item))
    .branch(dptree::case![Command::Favorites].endpoint(handle_favorites))
    .branch(dptree::case![Command::Mybids].endpoint(handle_my_bids))
    .branch(dptree::case![Command::Addcat { name }].endpoint(handle_add_category))
    .branch(dptree::case![Command::Additem].endpoint(start_additem))
    .branch(dptree::case![Command::Close { item_id }].endpoint(handle_close_item))
}

#[instrument(skip(bot))]
async fn handle_help(bot: Bot, msg: Message) -> HandlerResult {
  let text = Command::descriptions();
  bot.send_message(msg.chat.id, text.to_string()).await?;
  Ok(())
}

#[instrument(skip(bot, ctx))]
async fn handle_browse(bot: Bot, ctx: SharedContext, msg: Message) -> HandlerResult {
  send_categories(&bot, &ctx, msg.chat.id).await
}

#[instrument(skip(bot, ctx))]
async fn handle_item(bot: Bot, ctx: SharedContext, msg: Message, id: i64) -> HandlerResult {
  let viewer = msg.from.as_ref().map(|user| user.id.0 as i64);
  if !send_item(&bot, &ctx, msg.chat.id, id, viewer).await? {
    bot.send_message(msg.chat.id, "Item not found.").await?;
  }
  Ok(())
}

#[instrument(skip(bot, ctx))]
async fn handle_bid(bot: Bot, ctx: SharedContext, msg: Message, item_id: i64, amount: String) -> HandlerResult {
  let user = msg.from.as_ref().context("message missing sender")?;
  let bidder_id = user.id.0 as i64;
  let amount_text = amount.trim();

  match validate_bid(&ctx, item_id, amount_text).await {
    Ok((item, amount_cents)) => match ctx.db().place_bid(item_id, bidder_id, amount_cents).await {
      Ok(_) => {
        bot
          .send_message(
            msg.chat.id,
            format!("Bid placed at {} for item #{}.", format_cents(amount_cents), item_id),
          )
          .await?;
        let _ = notify_seller(&bot, &item, user, amount_cents).await;
        match send_item(&bot, &ctx, msg.chat.id, item_id, Some(bidder_id)).await {
          Ok(true) => {},
          Ok(false) => warn!(item_id, "item no longer available after bid"),
          Err(err) => warn!(error = %err, item_id, "failed to present item after bid"),
        }
      },
      Err(err) => {
        warn!(error = %err, item_id, bidder_id, "failed to store bid");
        bot
          .send_message(msg.chat.id, "Failed to place bid, try again later.")
          .await?;
      },
    },
    Err(BidError::Storage(err)) => {
      warn!(error = %err, item_id, bidder_id, "storage error while validating bid");
      bot
        .send_message(msg.chat.id, "Failed to place bid, try again later.")
        .await?;
    },
    Err(other) => {
      bot.send_message(msg.chat.id, other.user_message()).await?;
    },
  }

  Ok(())
}

#[instrument(skip(bot, ctx))]
async fn handle_favorites(bot: Bot, ctx: SharedContext, msg: Message) -> HandlerResult {
  let user = msg.from.as_ref().context("message missing sender")?;
  let user_id = user.id.0 as i64;
  let favorites = ctx.db().list_favorites(user_id).await?;

  if favorites.is_empty() {
    bot.send_message(msg.chat.id, "No favorites yet.").await?;
    return Ok(());
  }

  bot
    .send_message(msg.chat.id, format!("Favorites ({}):", favorites.len()))
    .await?;

  for item in favorites {
    if !send_item(&bot, &ctx, msg.chat.id, item.id, Some(user_id)).await? {
      warn!(item_id = item.id, "favorite item missing while rendering");
    }
  }

  Ok(())
}

#[instrument(skip(bot, ctx))]
async fn handle_my_bids(bot: Bot, ctx: SharedContext, msg: Message) -> HandlerResult {
  let user = msg.from.as_ref().context("message missing sender")?;
  let user_id = user.id.0 as i64;
  let bids = ctx.db().list_user_bid_items(user_id).await?;

  if bids.is_empty() {
    bot
      .send_message(msg.chat.id, "You have not placed any bids yet.")
      .await?;
    return Ok(());
  }

  bot
    .send_message(msg.chat.id, format!("Active bids ({} items):", bids.len()))
    .await?;

  for (item, _) in bids {
    if !send_item(&bot, &ctx, msg.chat.id, item.id, Some(user_id)).await? {
      warn!(item_id = item.id, "bid item missing while rendering");
    }
  }

  Ok(())
}

#[instrument(skip(bot, ctx))]
async fn handle_add_category(bot: Bot, ctx: SharedContext, msg: Message, name: String) -> HandlerResult {
  let admin = msg.from.context("message missing sender")?.id.0 as i64;
  if !ensure_admin(&ctx, admin, msg.chat.id, &bot).await? {
    return Ok(());
  }
  let trimmed = name.trim();
  if trimmed.is_empty() {
    bot
      .send_message(msg.chat.id, "Provide a non-empty category name.")
      .await?;
    return Ok(());
  }
  let (category, existing) = ensure_category(&ctx, trimmed).await?;
  if existing {
    bot
      .send_message(
        msg.chat.id,
        format!("Category already exists: {} (#{})", category.name, category.id),
      )
      .await?;
  } else {
    bot
      .send_message(
        msg.chat.id,
        format!("Category created: {} (#{})", category.name, category.id),
      )
      .await?;
  }
  Ok(())
}

#[instrument(skip(bot, ctx, dialogue))]
async fn start_additem(bot: Bot, dialogue: BotDialogue, ctx: SharedContext, msg: Message) -> HandlerResult {
  let from = msg.from.as_ref().context("message missing sender")?;
  let admin_id = from.id.0 as i64;
  if !ensure_admin(&ctx, admin_id, msg.chat.id, &bot).await? {
    return Ok(());
  }

  dialogue.reset().await?;

  let image_file_id = msg
    .reply_to_message()
    .and_then(|reply| reply.photo())
    .and_then(|photos| photos.last())
    .map(|photo| photo.file.id.clone());

  let draft = AddItemDraft::new(admin_id, image_file_id);
  dialogue.update(ConversationState::AddItem(draft)).await?;

  bot
    .send_message(msg.chat.id, "Enter category name (existing or new):")
    .await?;
  Ok(())
}

#[instrument(skip(bot, ctx, dialogue, draft))]
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
      .send_message(msg.chat.id, "Only the admin who started /additem can respond.")
      .await?;
    return Ok(());
  }

  let text = message_text(&msg).map(|t| t.trim()).filter(|t| !t.is_empty());
  let chat_id = msg.chat.id;

  match draft.stage {
    DraftStage::Category => {
      let Some(name) = text else {
        bot.send_message(chat_id, "Please provide a category name.").await?;
        return Ok(());
      };
      let (category, _) = ensure_category(&ctx, name).await?;
      draft.category_id = Some(category.id);
      draft.category_name = Some(category.name);
      draft.stage = DraftStage::Title;
      dialogue.update(ConversationState::AddItem(draft)).await?;
      bot.send_message(chat_id, "Enter item title:").await?;
    },
    DraftStage::Title => {
      let Some(title) = text else {
        bot.send_message(chat_id, "Please provide a title.").await?;
        return Ok(());
      };
      draft.title = Some(title.to_string());
      draft.stage = DraftStage::Description;
      dialogue.update(ConversationState::AddItem(draft)).await?;
      bot.send_message(chat_id, "Enter description (or '-' to skip):").await?;
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
      bot.send_message(chat_id, "Enter start price (e.g., 50.00):").await?;
    },
    DraftStage::StartPrice => {
      let Some(amount_text) = text else {
        bot
          .send_message(chat_id, "Provide a start price in 0.00 format.")
          .await?;
        return Ok(());
      };
      match parse_money_to_cents(amount_text) {
        Ok(value) => {
          draft.start_price = Some(value);
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
              draft.image_file_id.map(|e| e.to_string()).as_deref(),
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
          bot.send_message(chat_id, format!("Invalid price: {err}")).await?;
        },
      }
    },
  }

  Ok(())
}

#[instrument(skip(bot, ctx, dialogue))]
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

  if bidder_id != draft.bidder_tg_id {
    bot.send_message(chat_id, "Another bid is already in progress.").await?;
    return Ok(());
  }

  let Some(amount_text) = message_text(&msg).map(|t| t.trim()).filter(|t| !t.is_empty()) else {
    bot.send_message(chat_id, "Provide your bid in 0.00 format.").await?;
    return Ok(());
  };

  match validate_bid(&ctx, draft.item_id, amount_text).await {
    Ok((item, amount_cents)) => match ctx.db().place_bid(draft.item_id, bidder_id, amount_cents).await {
      Ok(_) => {
        dialogue.reset().await?;
        bot
          .send_message(
            chat_id,
            format!(
              "Bid placed at {} for item #{}.",
              format_cents(amount_cents),
              draft.item_id
            ),
          )
          .await?;
        let _ = notify_seller(&bot, &item, user, amount_cents).await;
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

#[instrument(skip(bot))]
async fn handle_idle_text(bot: Bot, msg: Message, state: ConversationState) -> HandlerResult {
  if matches!(state, ConversationState::Idle)
    && let Some(text) = msg.text()
  {
    if text.starts_with('/') {
      // unknown command, ignore to let telegram handle
    } else {
      bot
        .send_message(
          msg.chat.id,
          "I did not understand that. Use /help to see available commands.",
        )
        .await?;
    }
  }
  Ok(())
}

#[instrument(skip(bot, ctx))]
async fn handle_close_item(bot: Bot, ctx: SharedContext, msg: Message, item_id: i64) -> HandlerResult {
  let admin_id = msg.from.context("message missing sender")?.id.0 as i64;
  if !ensure_admin(&ctx, admin_id, msg.chat.id, &bot).await? {
    return Ok(());
  }
  ctx.db().close_item(item_id).await?;
  bot
    .send_message(msg.chat.id, format!("Item #{item_id} closed."))
    .await?;
  Ok(())
}

#[instrument(skip(bot, ctx, dialogue))]
async fn handle_callback_query(
  bot: Bot,
  ctx: SharedContext,
  query: CallbackQuery,
  dialogue: BotDialogue,
) -> HandlerResult {
  let mut callback_text: Option<String> = None;
  let user_id = query.from.id.0 as i64;
  let message_ctx = query.message.as_ref().map(|message| (message.chat().id, message.id()));

  if let Some(data) = query.data.as_deref() {
    if let Some((prefix, value)) = data.split_once(':') {
      match prefix {
        "cat" => {
          if let Ok(category_id) = value.parse::<i64>() {
            if let Some((chat_id, message_id)) = message_ctx {
              let categories = ctx.db().list_categories().await?;
              if let Some(category) = categories.into_iter().find(|c| c.id == category_id) {
                show_category_items_menu(&bot, &ctx, chat_id, message_id, category.id, category.name.as_str())
                  .await?;
              } else {
                callback_text = Some("Category not found".to_string());
              }
            }
          }
        },
        "item" => {
          if let Ok(item_id) = value.parse::<i64>() {
            if let Some((chat_id, _)) = message_ctx {
              if !send_item(&bot, &ctx, chat_id, item_id, Some(user_id)).await? {
                callback_text = Some("Item not found".to_string());
              }
            }
          }
        },
        "back" => {
          if value == "categories" {
            if let Some((chat_id, message_id)) = message_ctx {
              update_categories_menu(&bot, &ctx, chat_id, message_id).await?;
            }
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
                callback_text = Some("Auction is closed".to_string());
              },
              None => {
                callback_text = Some("Item not found".to_string());
              },
            }
          }
        },
        "fav" => {
          if let Some((action, item_str)) = value.split_once(':') {
            if let Ok(item_id) = item_str.parse::<i64>() {
              match action {
                "add" => {
                  ctx.db().add_favorite(user_id, item_id).await?;
                  callback_text = Some("Added to favorites".to_string());
                },
                "remove" => {
                  ctx.db().remove_favorite(user_id, item_id).await?;
                  callback_text = Some("Removed from favorites".to_string());
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
                {
                  if !matches!(err, RequestError::Api(ApiError::MessageNotModified)) {
                    return Err(err.into());
                  }
                }
              }
            }
          }
        },
        _ => {},
      }
    }
  }

  if let Some(text) = callback_text {
    bot.answer_callback_query(query.id).text(text).await?;
  } else {
    bot.answer_callback_query(query.id).await?;
  }
  Ok(())
}

async fn send_categories(bot: &Bot, ctx: &SharedContext, chat: ChatId) -> HandlerResult {
  let categories = ctx.db().list_categories().await?;
  if categories.is_empty() {
    bot.send_message(chat, "No categories yet. Check back soon.").await?;
    return Ok(());
  }
  let keyboard = build_categories_keyboard(&categories);
  bot
    .send_message(chat, "Choose a category:")
    .reply_markup(keyboard)
    .await?;
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
      .edit_message_text(chat, message_id, "No categories yet. Check back soon.")
      .reply_markup(InlineKeyboardMarkup::default());
    if let Err(err) = request.await {
      if !matches!(err, RequestError::Api(ApiError::MessageNotModified)) {
        return Err(err.into());
      }
    }
  } else {
    let keyboard = build_categories_keyboard(&categories);
    let request = bot
      .edit_message_text(chat, message_id, "Choose a category:")
      .reply_markup(keyboard);
    if let Err(err) = request.await {
      if !matches!(err, RequestError::Api(ApiError::MessageNotModified)) {
        return Err(err.into());
      }
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
  let text = if items.is_empty() {
    format!("Category: {category_name}\nNo items in this category yet.")
  } else {
    format!("Category: {category_name}\nSelect an item:")
  };
  let keyboard = build_items_keyboard(&items);
  let request = bot.edit_message_text(chat, message_id, text).reply_markup(keyboard);
  if let Err(err) = request.await {
    if !matches!(err, RequestError::Api(ApiError::MessageNotModified)) {
      return Err(err.into());
    }
  }
  Ok(())
}

fn build_categories_keyboard(categories: &[CategoryRow]) -> InlineKeyboardMarkup {
  InlineKeyboardMarkup::new(
    categories
      .chunks(2)
      .map(|row| {
        row
          .iter()
          .map(|category| InlineKeyboardButton::callback(category.name.clone(), format!("cat:{}", category.id)))
          .collect::<Vec<_>>()
      })
      .collect::<Vec<_>>(),
  )
}

fn build_items_keyboard(items: &[ItemRow]) -> InlineKeyboardMarkup {
  let mut rows = Vec::new();
  for item in items {
    rows.push(vec![InlineKeyboardButton::callback(
      truncate_button_text(&item.title, 32),
      format!("item:{}", item.id),
    )]);
  }
  rows.push(vec![InlineKeyboardButton::callback(
    "< Back".to_string(),
    "back:categories".to_string(),
  )]);
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

  if let Some(image) = item.image_file_id.clone() {
    bot
      .send_photo(chat, InputFile::file_id(image))
      .caption(text)
      .parse_mode(ParseMode::MarkdownV2)
      .reply_markup(keyboard)
      .await?;
  } else {
    bot
      .send_message(chat, text)
      .parse_mode(ParseMode::MarkdownV2)
      .reply_markup(keyboard)
      .await?;
  }

  Ok(true)
}

fn render_item_message(item: &ItemRow, best: Option<i64>, viewer: Option<&ItemViewerContext>) -> String {
  let escaped_id = markdown::escape(&format!("#{}", item.id));
  let escaped_title = markdown::escape(&item.title);
  let escaped_start = markdown::escape(&format_cents(item.start_price));

  let mut text = format!("*{}* — *{}*", escaped_id, escaped_title);

  if let Some(description) = item.description.as_deref() {
    if !description.trim().is_empty() {
      let escaped_description = markdown::escape(description);
      text.push_str(&format!("\n\n{}", escaped_description));
    }
  }

  text.push_str(&format!("\n\nStart: {}", escaped_start));

  if let Some(best_bid) = best {
    let escaped_best = markdown::escape(&format_cents(best_bid));
    text.push_str(&format!("\nCurrent best: {}", escaped_best));
  }

  if let Some(viewer_ctx) = viewer {
    if let Some(user_bid) = viewer_ctx.user_best_bid {
      let line = markdown::escape(&format!("Your top bid: {}", format_cents(user_bid)));
      text.push_str(&format!("\n{}", line));
    }
    if viewer_ctx.is_favorite {
      let line = markdown::escape("Saved to favorites");
      text.push_str(&format!("\n{}", line));
    }
  }

  text.push_str(&format!("\nStatus: {}", if item.is_open { "OPEN" } else { "CLOSED" }));
  text
}

fn item_action_keyboard(item_id: i64, open: bool, viewer: Option<&ItemViewerContext>) -> InlineKeyboardMarkup {
  let mut row = Vec::new();
  if open {
    row.push(InlineKeyboardButton::callback("Place bid", format!("bid:{item_id}")));
  }

  if let Some(viewer_ctx) = viewer {
    let (label, action) = if viewer_ctx.is_favorite {
      ("Remove favorite", "fav:remove")
    } else {
      ("Add favorite", "fav:add")
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

async fn notify_seller(bot: &Bot, item: &ItemRow, user: &User, amount_cents: i64) -> Result<()> {
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

async fn ensure_admin(ctx: &SharedContext, user_id: i64, chat: ChatId, bot: &Bot) -> Result<bool> {
  if ctx.is_admin(user_id) {
    return Ok(true);
  }
  bot.send_message(chat, "This command is restricted to admins.").await?;
  Ok(false)
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

async fn validate_bid(ctx: &SharedContext, item_id: i64, amount: &str) -> Result<(ItemRow, i64), BidError> {
  let amount_cents = parse_money_to_cents(amount)?;
  let item = ctx.db().get_item(item_id).await?.ok_or(BidError::NotFound)?;
  if !item.is_open {
    return Err(BidError::Closed);
  }
  if let Some(best) = ctx.db().best_bid_for_item(item_id).await? {
    if amount_cents <= best {
      return Err(BidError::TooLow(best));
    }
  } else if amount_cents < item.start_price {
    return Err(BidError::BelowStart(item.start_price));
  }
  Ok((item, amount_cents))
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

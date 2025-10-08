use teloxide::utils::command::BotCommands;

#[derive(BotCommands, Clone, Debug)]
#[command(rename_rule = "lowercase", description = "Available commands:")]
pub enum Command {
  /// Show the help text
  Help,
  /// Alias for /help
  Start,
  /// Browse available categories
  Browse,
  /// Show your saved items
  Favorites,
  /// Show items you have bid on
  Mybids,
  /// Show details for an item: /item <id>
  Item { id: i64 },
  /// Place a bid: /bid <item_id> <amount>
  #[command(parse_with = "split")]
  Bid { item_id: i64, amount: String },
  /// Admin: add a category
  Addcat { name: String },
  /// Admin: interactive item creation flow
  Additem,
  /// Admin: close an item: /close <item_id>
  Close { item_id: i64 },
}

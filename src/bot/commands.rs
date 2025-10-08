use teloxide::utils::command::BotCommands;

#[derive(BotCommands, Clone, Debug)]
#[command(rename_rule = "lowercase", description = "Available commands:")]
pub enum Command {
  /// Open the main menu
  Start,
  /// Show the help text
  Help,
}

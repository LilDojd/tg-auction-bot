use teloxide::dispatching::dialogue::InMemStorage;

pub mod commands;
pub mod context;
pub mod handlers;
pub mod state;

pub type HandlerResult = anyhow::Result<()>;
pub type DialogueStorage = InMemStorage<state::ConversationState>;

pub use commands::Command;
pub use context::AppContext;
pub use handlers::build_schema;

use std::env;

use anyhow::Context;
use anyhow::Result;

#[derive(Debug, Clone)]
pub struct Config {
  pub bot_token: String,
  pub database_url: String,
  pub admins: Vec<i64>,
}

impl Config {
  pub fn from_env() -> Result<Self> {
    let bot_token = env::var("BOT_TOKEN")
      .or_else(|_| env::var("TELOXIDE_TOKEN"))
      .context("BOT_TOKEN or TELOXIDE_TOKEN must be set")?;
    let database_url = env::var("DATABASE_URL").context("DATABASE_URL must be set")?;
    let admins_raw = env::var("ADMIN_IDS").unwrap_or_default();
    let admins = parse_admins(&admins_raw);
    Ok(Self {
      bot_token,
      database_url,
      admins,
    })
  }
}

fn parse_admins(raw: &str) -> Vec<i64> {
  raw
    .split(',')
    .filter_map(|id| {
      let trimmed = id.trim();
      if trimmed.is_empty() {
        return None;
      }
      match trimmed.parse::<i64>() {
        Ok(value) => Some(value),
        Err(err) => {
          tracing::warn!(value = trimmed, error = %err, "invalid ADMIN_IDS entry");
          None
        },
      }
    })
    .collect()
}

#[cfg(test)]
mod tests {
  use super::parse_admins;

  #[test]
  fn parses_valid_admins() {
    let admins = parse_admins("1, 2 ,3");
    assert_eq!(admins, vec![1, 2, 3]);
  }

  #[test]
  fn skips_invalid_entries() {
    let admins = parse_admins("42,abc,  7");
    assert_eq!(admins, vec![42, 7]);
  }

  #[test]
  fn empty_input_yields_empty_list() {
    let admins = parse_admins("");
    assert!(admins.is_empty());
  }
}

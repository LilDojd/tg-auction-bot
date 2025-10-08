use anyhow::Result;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt;

pub fn init() -> Result<()> {
  let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("debug"));
  fmt().with_env_filter(env_filter).with_target(true).init();
  Ok(())
}

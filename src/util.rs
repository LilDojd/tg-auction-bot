use once_cell::sync::Lazy;
use regex::Regex;
use thiserror::Error;

static PRICE_PATTERN: Lazy<Regex> = Lazy::new(|| Regex::new(r"^\d+(?:\.\d{1,2})?$").expect("valid regex"));

#[derive(Debug, Error, PartialEq, Eq)]
pub enum MoneyError {
  #[error("amount must match 0.00 format")]
  InvalidFormat,
  #[error("amount exceeds supported range")]
  OutOfRange,
}

pub fn parse_money_to_cents(input: &str) -> Result<i64, MoneyError> {
  if !PRICE_PATTERN.is_match(input.trim()) {
    return Err(MoneyError::InvalidFormat);
  }

  let mut parts = input.trim().split('.');
  let major = parts
    .next()
    .and_then(|p| p.parse::<i64>().ok())
    .ok_or(MoneyError::InvalidFormat)?;

  let minor = match parts.next() {
    None => 0,
    Some(minor) => {
      if minor.len() == 1 {
        (minor.to_owned() + "0")
          .parse::<i64>()
          .map_err(|_| MoneyError::OutOfRange)?
      } else {
        minor[.. 2].parse::<i64>().map_err(|_| MoneyError::OutOfRange)?
      }
    },
  };

  major
    .checked_mul(100)
    .and_then(|value| value.checked_add(minor))
    .ok_or(MoneyError::OutOfRange)
}

pub fn format_cents(amount: i64) -> String {
  format!("AED {:.2}", (amount as f64) / 100.0)
}

#[cfg(test)]
mod tests {
  use super::MoneyError;
  use super::format_cents;
  use super::parse_money_to_cents;

  #[test]
  fn parses_valid_amounts() {
    assert_eq!(parse_money_to_cents("10"), Ok(1000));
    assert_eq!(parse_money_to_cents("10.5"), Ok(1050));
    assert_eq!(parse_money_to_cents("10.55"), Ok(1055));
  }

  #[test]
  fn rejects_invalid_formats() {
    assert_eq!(parse_money_to_cents("abc"), Err(MoneyError::InvalidFormat));
    assert_eq!(parse_money_to_cents("10.555"), Err(MoneyError::InvalidFormat));
  }

  #[test]
  fn formats_currency() {
    assert_eq!(format_cents(1234), "AED 12.34");
  }
}

use chrono::DateTime;
use chrono::Utc;
use serde::Deserialize;
use serde::Serialize;
use teloxide::types::FileId;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct UserRow {
  pub id: i64, // tg id
  pub username: Option<String>,
  pub first_name: Option<String>,
  pub last_name: Option<String>,
  pub notifications_disabled: bool,
  pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CategoryRow {
  pub id: i64,
  pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ItemRow {
  pub id: i64,
  pub seller_tg_id: i64,
  pub category_id: i64,
  pub title: String,
  pub description: Option<String>,
  pub start_price: i64,
  pub image_file_id: Option<FileId>,
  pub is_open: bool,
  pub is_new: bool,
  pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct BidRow {
  pub id: i64,
  pub item_id: i64,
  pub bidder_tg_id: i64,
  pub amount: i64,
  pub created_at: DateTime<Utc>,
}

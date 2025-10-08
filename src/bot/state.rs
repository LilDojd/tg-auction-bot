use serde::Deserialize;
use serde::Serialize;
use teloxide::types::FileId;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case", tag = "kind", content = "data")]
pub enum ConversationState {
  #[default]
  Idle,
  AddItem(AddItemDraft),
  PlaceBid(BidDraft),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AddItemDraft {
  pub stage: DraftStage,
  pub seller_tg_id: i64,
  pub image_file_id: Option<FileId>,
  pub category_id: Option<i64>,
  pub category_name: Option<String>,
  pub title: Option<String>,
  pub description: Option<String>,
  pub start_price: Option<i64>,
}

impl AddItemDraft {
  pub fn new(seller_tg_id: i64, image_file_id: Option<FileId>) -> Self {
    Self {
      stage: DraftStage::Category,
      seller_tg_id,
      image_file_id,
      category_id: None,
      category_name: None,
      title: None,
      description: None,
      start_price: None,
    }
  }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum DraftStage {
  Category,
  Title,
  Description,
  StartPrice,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BidDraft {
  pub item_id: i64,
  pub bidder_tg_id: i64,
}

#[cfg(test)]
mod tests {
  use super::AddItemDraft;
  use super::DraftStage;

  #[test]
  fn new_draft_starts_with_category_stage() {
    let draft = AddItemDraft::new(1, None);
    assert_eq!(draft.stage, DraftStage::Category);
    assert_eq!(draft.seller_tg_id, 1);
    assert!(draft.image_file_id.is_none());
  }
}

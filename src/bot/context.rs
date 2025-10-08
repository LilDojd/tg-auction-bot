use std::collections::HashSet;

use crate::db::Db;

#[derive(Clone)]
pub struct AppContext {
  db: Db,
  admins: HashSet<i64>,
}

impl AppContext {
  pub fn new(db: Db, admins: Vec<i64>) -> Self {
    Self {
      db,
      admins: admins.into_iter().collect(),
    }
  }

  pub fn db(&self) -> &Db {
    &self.db
  }

  pub fn is_admin(&self, tg_id: i64) -> bool {
    self.admins.contains(&tg_id)
  }
}

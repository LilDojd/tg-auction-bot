use crate::models::CategoryRow;
use crate::models::ItemRow;
use anyhow::Result;
use sqlx::Pool;
use sqlx::Postgres;
use sqlx::Row;
use sqlx::migrate::Migrator;
use sqlx::postgres::PgPoolOptions;
use teloxide::types::FileId;
use tracing::instrument;

pub static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

#[derive(Clone)]
pub struct Db {
  pool: Pool<Postgres>,
}

impl Db {
  pub async fn connect(database_url: &str) -> Result<Self> {
    let pool = PgPoolOptions::new().max_connections(10).connect(database_url).await?;
    MIGRATOR.run(&pool).await?;
    Ok(Self { pool })
  }

  #[allow(dead_code)]
  pub fn pool(&self) -> &Pool<Postgres> {
    &self.pool
  }

  #[allow(dead_code)]
  #[instrument(skip(self))]
  pub async fn upsert_user(
    &self,
    id: i64,
    username: Option<String>,
    first_name: Option<String>,
    last_name: Option<String>,
  ) -> Result<()> {
    sqlx::query!(
      r#"
      INSERT INTO users (id, username, first_name, last_name)
      VALUES ($1, $2, $3, $4)
      ON CONFLICT (id) DO UPDATE SET
        username = EXCLUDED.username,
        first_name = EXCLUDED.first_name,
        last_name = EXCLUDED.last_name
      "#,
      id,
      username,
      first_name,
      last_name
    )
    .execute(&self.pool)
    .await?;
    Ok(())
  }

  #[instrument(skip(self))]
  pub async fn list_categories(&self) -> Result<Vec<CategoryRow>> {
    let rows = sqlx::query!(r#"SELECT id, name FROM categories ORDER BY name COLLATE "C""#)
      .fetch_all(&self.pool)
      .await?;
    Ok(
      rows
        .into_iter()
        .map(|row| CategoryRow {
          id: row.id,
          name: row.name,
        })
        .collect(),
    )
  }

  #[instrument(skip(self))]
  pub async fn find_category_by_name(&self, name: &str) -> Result<Option<CategoryRow>> {
    let row = sqlx::query!(
      r#"SELECT id, name FROM categories WHERE LOWER(name) = LOWER($1) LIMIT 1"#,
      name
    )
    .fetch_optional(&self.pool)
    .await?;
    Ok(row.map(|row| CategoryRow {
      id: row.id,
      name: row.name,
    }))
  }

  #[instrument(skip(self))]
  pub async fn create_category(&self, name: &str) -> Result<i64> {
    let id = sqlx::query_scalar!(r#"INSERT INTO categories (name) VALUES ($1) RETURNING id"#, name)
      .fetch_one(&self.pool)
      .await?;
    Ok(id)
  }

  #[instrument(skip(self))]
  pub async fn create_item(
    &self,
    seller_tg_id: i64,
    category_id: i64,
    title: &str,
    description: Option<&str>,
    start_price: i64,
    image_file_ids: &[String],
  ) -> Result<i64> {
    let cover_image = image_file_ids.first().map(|id| id.as_str());
    let id = sqlx::query_scalar!(
      r#"
      INSERT INTO items (seller_tg_id, category_id, title, description, start_price, image_file_id, is_new)
      VALUES ($1, $2, $3, $4, $5, $6, TRUE)
      RETURNING id
      "#,
      seller_tg_id,
      category_id,
      title,
      description,
      start_price,
      cover_image
    )
    .fetch_one(&self.pool)
    .await?;

    if !image_file_ids.is_empty() {
      for (position, file_id) in image_file_ids.iter().enumerate() {
        sqlx::query!(
          r#"
          INSERT INTO item_images (item_id, file_id, position)
          VALUES ($1, $2, $3)
          "#,
          id,
          file_id,
          position as i32,
        )
        .execute(&self.pool)
        .await?;
      }
    }
    Ok(id)
  }

  #[instrument(skip(self))]
  pub async fn list_items_by_category(&self, category_id: i64) -> Result<Vec<ItemRow>> {
    let rows = sqlx::query!(
      r#"
      SELECT
        id,
        seller_tg_id,
        category_id,
        title,
        description,
        start_price,
        image_file_id,
        is_open,
        is_new,
        created_at
      FROM items
      WHERE category_id = $1
      ORDER BY created_at DESC
      "#,
      category_id
    )
    .fetch_all(&self.pool)
    .await?;
    Ok(
      rows
        .into_iter()
        .map(|row| ItemRow {
          id: row.id,
          seller_tg_id: row.seller_tg_id,
          category_id: row.category_id,
          title: row.title,
          description: row.description,
          start_price: row.start_price,
          image_file_id: row.image_file_id.map(|i| i.into()),
          is_open: row.is_open,
          is_new: row.is_new,
          created_at: row.created_at,
        })
        .collect(),
    )
  }

  #[instrument(skip(self))]
  pub async fn get_item(&self, item_id: i64) -> Result<Option<ItemRow>> {
    let row = sqlx::query!(
      r#"
      SELECT
        id,
        seller_tg_id,
        category_id,
        title,
        description,
        start_price,
        image_file_id,
        is_open,
        is_new,
        created_at
      FROM items
      WHERE id = $1
      "#,
      item_id
    )
    .fetch_optional(&self.pool)
    .await?;
    Ok(row.map(|row| ItemRow {
      id: row.id,
      seller_tg_id: row.seller_tg_id,
      category_id: row.category_id,
      title: row.title,
      description: row.description,
      start_price: row.start_price,
      image_file_id: row.image_file_id.map(|i| i.into()),
      is_open: row.is_open,
      is_new: row.is_new,
      created_at: row.created_at,
    }))
  }

  #[instrument(skip(self))]
  pub async fn list_item_images(&self, item_id: i64) -> Result<Vec<FileId>> {
    let rows = sqlx::query!(
      r#"
      SELECT file_id
      FROM item_images
      WHERE item_id = $1
      ORDER BY position ASC, id ASC
      "#,
      item_id
    )
    .fetch_all(&self.pool)
    .await?;

    Ok(rows.into_iter().map(|row| row.file_id.into()).collect())
  }

  #[instrument(skip(self))]
  pub async fn best_bid_for_item(&self, item_id: i64) -> Result<Option<i64>> {
    let value = sqlx::query_scalar!(
      r#"SELECT amount FROM bids WHERE item_id = $1 ORDER BY amount DESC LIMIT 1"#,
      item_id
    )
    .fetch_optional(&self.pool)
    .await?;
    Ok(value)
  }

  #[instrument(skip(self))]
  pub async fn best_bid_with_bidder(&self, item_id: i64) -> Result<Option<(i64, i64)>> {
    let row = sqlx::query!(
      r#"
      SELECT bidder_tg_id, amount
      FROM bids
      WHERE item_id = $1
      ORDER BY amount DESC, created_at ASC
      LIMIT 1
      "#,
      item_id
    )
    .fetch_optional(&self.pool)
    .await?;

    Ok(row.map(|row| (row.bidder_tg_id, row.amount)))
  }

  #[instrument(skip(self))]
  pub async fn user_best_bid_for_item(&self, item_id: i64, user_id: i64) -> Result<Option<i64>> {
    let value = sqlx::query_scalar::<_, i64>(
      "SELECT amount FROM bids WHERE item_id = $1 AND bidder_tg_id = $2 ORDER BY amount DESC LIMIT 1",
    )
    .bind(item_id)
    .bind(user_id)
    .fetch_optional(&self.pool)
    .await?;
    Ok(value)
  }

  #[instrument(skip(self))]
  pub async fn place_bid(&self, item_id: i64, bidder_tg_id: i64, amount: i64) -> Result<i64> {
    let id = sqlx::query_scalar!(
      r#"
      INSERT INTO bids (item_id, bidder_tg_id, amount)
      VALUES ($1, $2, $3)
      RETURNING id
      "#,
      item_id,
      bidder_tg_id,
      amount
    )
    .fetch_one(&self.pool)
    .await?;
    Ok(id)
  }

  #[instrument(skip(self))]
  pub async fn list_user_bid_items(&self, user_id: i64) -> Result<Vec<(ItemRow, i64)>> {
    let rows = sqlx::query(
      r#"
      SELECT DISTINCT ON (b.item_id)
        i.id,
        i.seller_tg_id,
        i.category_id,
        i.title,
        i.description,
        i.start_price,
        i.image_file_id,
        i.is_open,
        i.is_new,
        i.created_at,
        b.amount
      FROM bids b
      INNER JOIN items i ON i.id = b.item_id
      WHERE b.bidder_tg_id = $1
      ORDER BY b.item_id, b.amount DESC
      "#,
    )
    .bind(user_id)
    .fetch_all(&self.pool)
    .await?;

    let items = rows
      .into_iter()
      .map(|row| {
        let item = ItemRow {
          id: row.get("id"),
          seller_tg_id: row.get("seller_tg_id"),
          category_id: row.get("category_id"),
          title: row.get("title"),
          description: row.get("description"),
          start_price: row.get("start_price"),
          image_file_id: row.get::<Option<String>, _>("image_file_id").map(Into::into),
          is_open: row.get("is_open"),
          is_new: row.get("is_new"),
          created_at: row.get("created_at"),
        };
        let amount = row.get("amount");
        (item, amount)
      })
      .collect();
    Ok(items)
  }

  #[instrument(skip(self))]
  pub async fn close_item(&self, item_id: i64) -> Result<()> {
    sqlx::query!(r#"UPDATE items SET is_open = FALSE WHERE id = $1"#, item_id)
      .execute(&self.pool)
      .await?;
    Ok(())
  }

  #[instrument(skip(self))]
  pub async fn list_item_bidder_ids(&self, item_id: i64) -> Result<Vec<i64>> {
    let bidders = sqlx::query_scalar!(r#"SELECT DISTINCT bidder_tg_id FROM bids WHERE item_id = $1"#, item_id)
      .fetch_all(&self.pool)
      .await?;
    Ok(bidders)
  }

  #[instrument(skip(self))]
  pub async fn list_item_favorite_user_ids(&self, item_id: i64) -> Result<Vec<i64>> {
    let favorites = sqlx::query_scalar!(r#"SELECT DISTINCT user_id FROM favorites WHERE item_id = $1"#, item_id)
      .fetch_all(&self.pool)
      .await?;
    Ok(favorites)
  }

  #[instrument(skip(self))]
  pub async fn delete_item(&self, item_id: i64) -> Result<bool> {
    let result = sqlx::query!(r#"DELETE FROM items WHERE id = $1"#, item_id)
      .execute(&self.pool)
      .await?;
    Ok(result.rows_affected() > 0)
  }

  #[instrument(skip(self))]
  pub async fn delete_category(&self, category_id: i64) -> Result<bool> {
    let result = sqlx::query!(r#"DELETE FROM categories WHERE id = $1"#, category_id)
      .execute(&self.pool)
      .await?;
    Ok(result.rows_affected() > 0)
  }

  #[instrument(skip(self))]
  pub async fn add_favorite(&self, user_id: i64, item_id: i64) -> Result<()> {
    sqlx::query(
      r#"
      INSERT INTO favorites (user_id, item_id)
      VALUES ($1, $2)
      ON CONFLICT (user_id, item_id) DO NOTHING
      "#,
    )
    .bind(user_id)
    .bind(item_id)
    .execute(&self.pool)
    .await?;
    Ok(())
  }

  #[instrument(skip(self))]
  pub async fn remove_favorite(&self, user_id: i64, item_id: i64) -> Result<()> {
    sqlx::query("DELETE FROM favorites WHERE user_id = $1 AND item_id = $2")
      .bind(user_id)
      .bind(item_id)
      .execute(&self.pool)
      .await?;
    Ok(())
  }

  #[instrument(skip(self))]
  pub async fn is_favorite(&self, user_id: i64, item_id: i64) -> Result<bool> {
    let exists =
      sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM favorites WHERE user_id = $1 AND item_id = $2)")
        .bind(user_id)
        .bind(item_id)
        .fetch_one(&self.pool)
        .await?;
    Ok(exists)
  }

  #[instrument(skip(self))]
  pub async fn list_favorites(&self, user_id: i64) -> Result<Vec<ItemRow>> {
    let rows = sqlx::query(
      r#"
      SELECT i.id,
             i.seller_tg_id,
             i.category_id,
             i.title,
             i.description,
             i.start_price,
             i.image_file_id,
             i.is_open,
             i.is_new,
             i.created_at
      FROM favorites f
      INNER JOIN items i ON i.id = f.item_id
      WHERE f.user_id = $1
      ORDER BY f.created_at DESC
      "#,
    )
    .bind(user_id)
    .fetch_all(&self.pool)
    .await?;

    let items = rows
      .into_iter()
      .map(|row| ItemRow {
        id: row.get("id"),
        seller_tg_id: row.get("seller_tg_id"),
        category_id: row.get("category_id"),
        title: row.get("title"),
        description: row.get("description"),
        start_price: row.get("start_price"),
        image_file_id: row.get::<Option<String>, _>("image_file_id").map(Into::into),
        is_open: row.get("is_open"),
        is_new: row.get("is_new"),
        created_at: row.get("created_at"),
      })
      .collect();
    Ok(items)
  }

  #[instrument(skip(self))]
  pub async fn list_user_ids(&self) -> Result<Vec<i64>> {
    let ids = sqlx::query_scalar!(r#"SELECT id FROM users"#)
      .fetch_all(&self.pool)
      .await?;
    Ok(ids)
  }

  #[instrument(skip(self))]
  pub async fn list_new_items(&self) -> Result<Vec<ItemRow>> {
    let rows = sqlx::query!(
      r#"
      SELECT
        id,
        seller_tg_id,
        category_id,
        title,
        description,
        start_price,
        image_file_id,
        is_open,
        is_new,
        created_at
      FROM items
      WHERE is_new = TRUE
      ORDER BY created_at DESC
      "#
    )
    .fetch_all(&self.pool)
    .await?;

    Ok(
      rows
        .into_iter()
        .map(|row| ItemRow {
          id: row.id,
          seller_tg_id: row.seller_tg_id,
          category_id: row.category_id,
          title: row.title,
          description: row.description,
          start_price: row.start_price,
          image_file_id: row.image_file_id.map(|i| i.into()),
          is_open: row.is_open,
          is_new: row.is_new,
          created_at: row.created_at,
        })
        .collect(),
    )
  }

  #[instrument(skip(self))]
  pub async fn clear_new_item_flags(&self, item_ids: &[i64]) -> Result<()> {
    if item_ids.is_empty() {
      return Ok(());
    }

    let ids: Vec<i64> = item_ids.to_vec();
    sqlx::query!("UPDATE items SET is_new = FALSE WHERE id = ANY($1)", &ids)
      .execute(&self.pool)
      .await?;
    Ok(())
  }
}

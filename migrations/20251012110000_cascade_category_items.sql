ALTER TABLE items
  DROP CONSTRAINT IF EXISTS items_category_id_fkey,
  ADD CONSTRAINT items_category_id_fkey
    FOREIGN KEY (category_id)
    REFERENCES categories(id)
    ON DELETE CASCADE;

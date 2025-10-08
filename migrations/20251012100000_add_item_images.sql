CREATE TABLE IF NOT EXISTS item_images (
    id          BIGSERIAL PRIMARY KEY,
    item_id     BIGINT NOT NULL REFERENCES items(id) ON DELETE CASCADE,
    file_id     TEXT NOT NULL,
    position    INT NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_item_images_item_position
    ON item_images(item_id, position);

INSERT INTO item_images (item_id, file_id, position)
SELECT id, image_file_id, 0
FROM items
WHERE image_file_id IS NOT NULL;

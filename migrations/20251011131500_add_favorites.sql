CREATE TABLE IF NOT EXISTS favorites (
    user_id     BIGINT NOT NULL,
    item_id     BIGINT NOT NULL REFERENCES items(id) ON DELETE CASCADE,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, item_id)
);

CREATE INDEX IF NOT EXISTS idx_favorites_user_created ON favorites(user_id, created_at DESC);

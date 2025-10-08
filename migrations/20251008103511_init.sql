-- Users who interact with the bot
CREATE TABLE IF NOT EXISTS users (
    id              BIGINT PRIMARY KEY,
    username        TEXT,
    first_name      TEXT,
    last_name       TEXT,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS categories (
    id          BIGSERIAL PRIMARY KEY,
    name        TEXT NOT NULL UNIQUE
);

CREATE TABLE IF NOT EXISTS items (
    id              BIGSERIAL PRIMARY KEY,
    seller_tg_id    BIGINT NOT NULL,
    category_id     BIGINT NOT NULL REFERENCES categories(id) ON DELETE RESTRICT,
    title           TEXT NOT NULL,
    description     TEXT,
    start_price     BIGINT NOT NULL,
    image_file_id   TEXT,
    is_open         BOOLEAN NOT NULL DEFAULT TRUE,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS bids (
    id              BIGSERIAL PRIMARY KEY,
    item_id         BIGINT NOT NULL REFERENCES items(id) ON DELETE CASCADE,
    bidder_tg_id    BIGINT NOT NULL,
    amount          BIGINT NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_items_category ON items(category_id);
CREATE INDEX IF NOT EXISTS idx_bids_item ON bids(item_id);
CREATE INDEX IF NOT EXISTS idx_bids_item_amount ON bids(item_id, amount DESC);

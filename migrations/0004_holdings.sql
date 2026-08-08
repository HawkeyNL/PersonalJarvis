-- Portfolio holdings, entered manually (no market-data provider yet).
-- Money and quantities use NUMERIC (never floating point).

create table holdings (
    id         uuid           primary key,
    user_id    uuid           not null references users (id) on delete cascade,
    symbol     text           not null,
    quantity   numeric(20, 8) not null check (quantity > 0),
    avg_cost   numeric(20, 8) not null check (avg_cost >= 0),
    currency   text           not null default 'EUR',
    created_at timestamptz    not null default now(),
    updated_at timestamptz    not null default now()
);

create index holdings_user_id_idx on holdings (user_id);

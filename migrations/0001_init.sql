-- Baseline schema for Jarvis.
-- Domain-specific tables (identity, portfolio, ...) get their own migrations.
-- This first migration only records that the schema has been initialised, so
-- migration wiring can be verified end-to-end.

create table if not exists system_info (
    id             smallint    primary key default 1,
    schema_name    text        not null    default 'jarvis',
    initialised_at timestamptz not null    default now(),
    constraint system_info_singleton check (id = 1)
);

insert into system_info (id)
values (1)
on conflict (id) do nothing;

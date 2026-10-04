-- Schema remoto do Todo Taskbar (Supabase/Postgres).
--
-- O cliente mantém uma cópia local em SQLite e reconcilia com estas tabelas por
-- "last-write-wins" usando `updated_at`. Exclusões são marcadas com `deleted`
-- (tombstone) em vez de apagadas, para não "ressuscitarem" no próximo sync.
--
-- Os ids são UUID gerados pelo cliente: dois dispositivos offline podem criar
-- itens ao mesmo tempo sem colidir.
--
-- Tudo é protegido por RLS: cada linha pertence a `auth.uid()`.

create extension if not exists pgcrypto;

create table if not exists public.tabs (
  id         uuid primary key default gen_random_uuid(),
  user_id    uuid not null default auth.uid() references auth.users (id) on delete cascade,
  name       text not null,
  position   bigint not null default 0,
  created_at timestamptz not null default now(),
  updated_at timestamptz not null default now(),
  deleted    boolean not null default false
);

create table if not exists public.todos (
  id           uuid primary key default gen_random_uuid(),
  user_id      uuid not null default auth.uid() references auth.users (id) on delete cascade,
  tab_id       uuid not null references public.tabs (id) on delete cascade,
  text         text not null,
  done         boolean not null default false,
  completed_at timestamptz,
  position     bigint not null default 0,
  created_at   timestamptz not null default now(),
  updated_at   timestamptz not null default now(),
  deleted      boolean not null default false
);

create index if not exists idx_tabs_user_updated  on public.tabs  (user_id, updated_at);
create index if not exists idx_todos_user_updated on public.todos (user_id, updated_at);
create index if not exists idx_todos_tab          on public.todos (tab_id);

alter table public.tabs  enable row level security;
alter table public.todos enable row level security;

create policy "tabs_owner" on public.tabs
  for all
  using (auth.uid() = user_id)
  with check (auth.uid() = user_id);

create policy "todos_owner" on public.todos
  for all
  using (auth.uid() = user_id)
  with check (auth.uid() = user_id);

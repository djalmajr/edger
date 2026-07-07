-- parameters-v2 greenfield schema.
-- Domínio inspirado no ClusterSpaceParameter do front-manager-api:
-- store de configuração por-tenant, hierárquico (árvore auto-referente), tipado.
create extension if not exists "pgcrypto";

create table if not exists cluster_space_parameter (
  id          uuid primary key default gen_random_uuid(),
  tenant_id   uuid not null,
  parent_id   uuid references cluster_space_parameter (id) on delete cascade,
  key         text not null,
  value       jsonb,
  value_type  text not null default 'string',
  created_at  timestamptz not null default now(),
  updated_at  timestamptz not null default now()
);

-- Unicidade de key por (tenant, parent). parent_id NULL = raiz; coalesce para um
-- sentinel para que chaves de raiz também sejam deduplicadas (NULLs seriam distintos).
create unique index if not exists uq_csp_tenant_parent_key
  on cluster_space_parameter (tenant_id, coalesce(parent_id, '00000000-0000-0000-0000-000000000000'::uuid), key);

create index if not exists idx_csp_tenant on cluster_space_parameter (tenant_id);
create index if not exists idx_csp_parent on cluster_space_parameter (parent_id);

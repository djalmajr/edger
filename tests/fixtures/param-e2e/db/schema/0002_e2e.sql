-- e2e observability tables + a deterministic seed. Applied by the release phase
-- (B3) on first boot; the run.sh assertions read these back through the worker
-- and directly.

-- Proof the release migration ran (run.sh checks this table exists + has a row).
create table if not exists e2e_release_marker (
  id serial primary key,
  ran_at timestamptz not null default now()
);
insert into e2e_release_marker default values;

-- Written by the worker's beforeunload handler on graceful recycle (B2 proof).
create table if not exists e2e_shutdown_log (
  id serial primary key,
  reason text,
  at timestamptz not null default now()
);

-- Deterministic parameter tree (fixed UUIDs) so run.sh can assert the query.
insert into cluster_space_parameter (id, tenant_id, parent_id, key, value, value_type)
values
  ('22222222-2222-2222-2222-222222222222', '11111111-1111-1111-1111-111111111111', null,
   'ui', '{}'::jsonb, 'group'),
  ('33333333-3333-3333-3333-333333333333', '11111111-1111-1111-1111-111111111111',
   '22222222-2222-2222-2222-222222222222', 'theme', '"dark"'::jsonb, 'string'),
  ('44444444-4444-4444-4444-444444444444', '11111111-1111-1111-1111-111111111111', null,
   'featureFlags', '{"beta":true}'::jsonb, 'json')
on conflict do nothing;

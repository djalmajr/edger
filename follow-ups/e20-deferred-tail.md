# Epic 20 — cauda deferida (P2/P3)

**Contexto:** o Epic 20 (endurecimento do runtime) entregou 9.5/12 stories —
todos os P0 e P1 mais rate-limit e o evento por-execução. As 3 stories abaixo
foram **deferidas por decisão explícita** (baixo ROI / deps pesadas /
não-validáveis fora de cluster ou collector). São itens P2/P3 que a análise
comparativa (vs Supabase/run2biz edge-runtimes) já classificara como cauda.

## 09 (parte OTLP) — exportador OTLP real

**O que falta:** linkar um exportador OTLP de traces atrás do stub em
`crates/edger-orchestrator/src/tracing_init.rs` (que hoje só emite warning quando
`OTEL_EXPORTER_OTLP_ENDPOINT` está setado). A parte de **evento por-execução**
já foi entregue (target `edger.dispatch`, PR #28) e alimenta o OTLP quando linkado.

**Como fazer:** cargo feature `otel` (opt-in, não infla o binário default) puxando
`opentelemetry` + `opentelemetry-otlp` + `tracing-opentelemetry`; refatorar
`init_tracing_from_env` para `Registry` + layers e adicionar o layer OTLP quando
a feature está on e o endpoint setado. Propagar `traceparent` no header injetado
ao worker.

**Por que deferido:** deps pesadas (tonic/grpc ou http+prost) e **o export só é
validável com um collector OTLP rodando** — sem collector, dá só para confirmar
que compila e linka. Observabilidade atual (métricas Prometheus + evento
`edger.dispatch` + `worker_errors` ring buffer) já cobre o caso comum.

## 10 — cron leader-election (multi-réplica)

**O que falta:** evitar execução duplicada de cron quando o chart escala para
N réplicas (hoje `cron.rs` é scheduler in-process por réplica). Só o líder
dispara os jobs.

**Como fazer:** leader-election via `coordination.k8s.io` Lease (holder identity =
pod name; acquire/renew), gated por env (ex.: `EDGER_CRON_LEADER_ELECTION=true`).
Alternativa leve sem o crate `kube`: acquire/renew do Lease via API do k8s com o
token do serviceaccount in-cluster. Não-líderes ficam idle. Manter cron
desligável (`EDGER_CRON_ENABLED`).

**Por que deferido:** só faz sentido em k8s multi-réplica e **não há cluster
disponível aqui para validar** a coordenação real; seria best-effort não-testado.
Em single-réplica (deploy atual) não há duplicação, então não morde hoje.

## 11 — sinais de lifecycle ao JS (beforeunload / waitUntil)

**O que falta:** (a) sinalizar `beforeunload`/drain com razão ao código JS antes
de reciclar/drenar um processo (para flush gracioso); (b) um `waitUntil` mínimo
opt-in para trabalho de fundo pós-resposta.

**Como fazer:** frame de controle do supervisor (`multiproc.rs`) → harness
(`multiproc_harness.mjs`) dispara um evento com deadline curto antes do kill.

**Por que deferido:** a **análise adversarial marcou o `waitUntil` como risco ao
ethos minimalista** — ele adia o ACK/recycle até promises resolverem, o que
interage perigosamente com a serialização "one request per process" (segura o
slot único) e com os limites de CPU/wall. O `beforeunload` isolado é de baixo
valor frente ao custo de adicionar contrato de lifecycle runtime↔user-code.
Preferir, se necessário, um único gancho opt-in com teto de tempo rígido.

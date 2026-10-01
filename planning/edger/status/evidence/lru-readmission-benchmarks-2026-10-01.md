# Readmissão LRU e benchmarks de versões — 2026-10-01

## Escopo
Correção local autorizada do bloqueio permanente de uma identidade app/versão após evicção por capacidade. Benchmarks após a correção; sem commit, publicação ou deploy. Worktree: `.worktrees/cpanel-act-warnings`.

## Estado
Correção implementada pelo pi/Qwen `keys-qwen` e revisão final aprovada pelo Sonnet `review-cpanel-claude`. Benchmarks executados pelo segundo pi/Qwen `cpanel-qwen`, ambos `lbvllm/qwen3.8-27b`, thinking high. Revisão final da metodologia aprovada, com zero achados e nenhum item obrigatório parcial. Concluído localmente, sem commit ou deploy.

## Limpeza de disco
Antes: volume de dados com aproximadamente 17 GiB disponíveis. Depois: 35 GiB disponíveis (diferença observada de aproximadamente 18 GiB; tamanho aparente removido é maior por características do filesystem).
Removidos somente `target/debug/incremental`, `/tmp/edger-review-luna-160758`, `/tmp/edger-review-luna-9eb6413-ax5waxp0` e `/tmp/edger-target-188`, todos descartáveis sem jobs ativos. Preservados código, worktrees, relatórios, dependências compiladas e runtime de preview. Novas checagens usam `CARGO_INCREMENTAL=0`.

## Critérios
- Readmissão automática após evicção, sem recycle/restart manual.
- Limite de grupos, isolamento por versão e requisições em andamento preservados.
- Processos removidos encerrados sem ficarem presos ao timer TTL.
- Regressão executada, sensibilidade ao mecanismo, gate Rust completo e revisão independente.
- Benchmarks reproduzíveis com seleção isolada e HTTP/Deno real, separando warm/cold/churn, erros e versões instaladas/acessadas.

## Referências
- `planning/edger/epics/04-worker-management/01-worker-pool-lru.md`
- `crates/edger-worker/src/pool.rs`
- `crates/edger-worker/src/lru.rs`
- `crates/edger-worker/tests/pool_lru.rs`

## Correção e provas locais
- Evicção por capacidade retira o grupo antigo, sem bloquear permanentemente a identidade app/versão. Nova requisição pode criar uma geração nova.
- Inserção concorrente preserva o primeiro grupo vencedor; grupos removidos recusam slots novos, acordam waiters e aguardam a requisição ativa antes de terminar o processo. Cleanup antigo não remove geração nova.
- Drain cancela o TTL do processo removido; prewarm verifica a marca de evicção sob o lock de dispatch antes de preparar.
- Regressão: 13 testes passaram. Na cópia com código anterior, 10 dos 11 casos aplicáveis falharam com `Evicted`; o caso que usa recycle como escape passou. O teste da API nova `GroupInsertOutcome` foi excluído somente da cópia antiga incompatível.
- Revisão final executou workspace test (66 blocos de resultado), clippy e fmt, além dos 13 casos focados. A sonda de prewarm com gancho somente na cópia produziu `prepares=0 / Ok(0)`; revertendo apenas a guarda, `prepares=1 / Ok(1)`.
- Ressalva P3: a guarda específica de prewarm tem contraprova executada na cópia, mas não um teste determinístico incorporado à suíte. Nenhum P0–P2 ou item obrigatório parcial ficou aberto na revisão final.
- Correção da prosa do implementador: dispatch já reserva slot e lock atomicamente; o prewarm antigo não agendava TTL. O cenário descrito como `Creating` voltando a `Idle` e esperando TTL não representa o mecanismo atual. A prova do Deno real virá do benchmark HTTP, separada dos isolates falsos dos testes de concorrência.

Relatórios locais do orquestrador: `.herdr-agents/wF/reports/fix-lru-readmission-20261001.md`, `review-lru-readmission-20261001.md` e `review-lru-prewarm-final-20261001.md` (na raiz do checkout principal). As fontes atuais e hashes constam da revisão final.

## Resultados medidos — execução final de 11:56–11:57
Release em macOS/Apple Silicon, loopback, requests sequenciais. Sem compilação durante as medições. Rust 1.98.0, Deno 2.9.7, Python 3.14.3. Tenant, weighted routing e OTEL desligados; runtime JS explicitamente `process`.

As tentativas anteriores ficaram preservadas em `/tmp/edger-version-bench-results-20261001/`. Após a revisão, o micro foi repetido com hash do binário medido antes/depois igual ao binário atual. O HTTP foi repetido com agregação que exclui respostas inválidas, registro de ambiente e blocos descritos corretamente como sequenciais. Somente as execuções atuais entram nas tabelas abaixo.

### HTTP completo warm (ms)
180 amostras por linha: três blocos de 60 no mesmo processo por fase, com 20 warmups por bloco excluídos. Servidor novo para cada V; pinned e default servem 1.0.0. Default promovido para versão não mais alta quando V > 1.

| Versões instaladas | Rota | p50 ms | p95 ms | p99 ms |
|---:|---|---:|---:|---:|
| 1 | hot-default | 0.204 | 0.542 | 1.015 |
| 1 | hot-pinned | 0.219 | 0.610 | 0.822 |
| 10 | hot-default | 0.238 | 1.056 | 1.625 |
| 10 | hot-pinned | 0.209 | 0.424 | 0.808 |
| 100 | hot-default | 0.162 | 0.334 | 0.839 |
| 100 | hot-pinned | 0.322 | 1.262 | 7.084 |

A variação não foi monotônica por V. O host estava compartilhado, com loadavg de cerca de 18 no início/fim do HTTP. Não se pode atribuir cada cauda ao número de versões ou estabelecer SLA. Não foram medidos throughput, concorrência, consumo máximo de memória ou produção. São blocos sequenciais no mesmo processo, não experimentos independentes.

### Seleção isolada de rota (µs, sem Deno/HTTP)
Pinned escolhe a última posição do bucket, diferente do alvo primeiro do HTTP. Fixture/default são carregados fora do trecho medido; 30.000 amostras por cenário até V100 e 6.000 em V1000, com warmup excluído.

| V | Rota | p50 µs | p95 µs | p99 µs |
|---:|---|---:|---:|---:|
| 1 | unpinned-latest | 0.375 | 0.500 | 1.083 |
| 1 | pinned | 0.333 | 0.458 | 0.833 |
| 1 | default-explicit | 0.375 | 0.459 | 0.542 |
| 10 | unpinned-latest | 0.750 | 0.833 | 0.916 |
| 10 | pinned | 0.500 | 0.584 | 1.250 |
| 10 | default-explicit | 0.709 | 0.833 | 1.750 |
| 100 | unpinned-latest | 3.750 | 4.041 | 8.208 |
| 100 | pinned | 1.292 | 1.458 | 2.750 |
| 100 | default-explicit | 3.500 | 3.708 | 3.916 |
| 1000 | unpinned-latest | 34.583 | 37.375 | 81.375 |
| 1000 | pinned | 8.584 | 9.375 | 17.459 |
| 1000 | default-explicit | 32.125 | 84.625 | 418.208 |

O custo de seleção aumenta com versões. O default explícito ainda executa o fallback semver avidamente em `unwrap_or(resolve_semver(...)?)`; não é lookup O(1). Com 1.000 versões, default p50 = 32,125 µs e pinned = 8,584 µs. Essa otimização adicional não faz parte da correção LRU.

| P (versões do mesmo plugin/base) | Worker passando pelos plugins: p50 µs | p95 µs | p99 µs |
|---:|---:|---:|---:|
| 1 | 0.458 | 1.083 | 1.209 |
| 10 | 3.042 | 3.500 | 7.875 |
| 100 | 198.875 | 346.500 | 950.708 |

Essa rota de worker não corresponde às bases de plugin e escaneia todos os candidatos. O predicado de habilitação procura a entrada por diretório para cada candidato; essa combinação é o próximo ponto de otimização indicado pela evidência. Rotas que correspondem à primeira base de plugin não exercitam esse pior caso. P representa versões do mesmo plugin, não P apps distintos.

### Readmissão com Deno real
40 versões acessadas, acima dos 32 grupos LRU. Readmissão de 1.0.0: HTTP 200, **56.503 ms** (um ponto frio), UUID diferente do sweep; próximo hit **0.533 ms** (um ponto warm), mesmo UUID da readmissão. Controle 1.0.39 manteve UUID do sweep e respondeu em 0.728 ms. `readmission_proven = true`. Não ocorreu bloqueio permanente 500.

**1.486 requisições finais**, incluindo warmups e sweep, com zero status divergente, erro, mismatch de versão/app, instância vazia, geração warm divergente ou retry. Instalar versões só aumenta metadados; os processos são criados sob demanda. O limite de 32 vale para grupos no cache, não para memória absoluta nem para todos os in-flight removidos que ainda precisam terminar.

### Reprodução e artefatos
Harnesses: `crates/edger-orchestrator/examples/version-routing-benchmark.rs` e `scripts/benchmark-version-http.py`. Comandos completos e hashes no relatório final do implementador `.herdr-agents/wF/reports/version-benchmarks-final-20261001.md` (checkout principal). JSON, CSV e proveniência preservados no diretório ao lado deste documento: `lru-readmission-benchmarks-2026-10-01/`. Micro usa V1/10/100/1000 e P1/10/100; HTTP usa V1/10/100 e churn40. Build:

```sh
CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=2 cargo +1.98.0 build --release -p edger-orchestrator --bin edger --example version-routing-benchmark
VRB_OUT=/tmp/routing.json target/release/examples/version-routing-benchmark
python3 scripts/benchmark-version-http.py --edger-bin "$(pwd)/target/release/edger" --deno-bin /opt/homebrew/bin/deno --out /tmp/http-results
```

A proveniência inclui binário medido, script/fonte, HEAD e dirty boolean, tooling, OS/arquitetura e loadavg antes/depois. Não há env dump ou segredo. A primeira medição micro sem hash do binário anterior foi substituída por execução auditável; nenhuma tentativa antiga foi mascarada.

### Gates e revisão
Gate Rust com exit real: workspace test (66 blocos de resultado), clippy `-D warnings` e fmt check passaram. Gates de planejamento: refinement, referências, layout, módulos, cPanel/WebIDE, 174 testes do cPanel e cargo check passaram; logs em `/tmp/edger-planning-followup-20261001/`. A emenda Python passou compile e sonda negativa de agregação/corpo, sem repetir suite Rust cujo código não mudou. Rerevisão final da metodologia aprovada: zero P0–P3 e nenhum item obrigatório parcial; relatório `.herdr-agents/wF/reports/review-version-benchmarks-final-20261001.md` no checkout principal. A ressalva P3 de cobertura determinística de prewarm da revisão do pool continua registrada, com sonda e contraprova executadas fora da árvore. Nenhuma prova remota nem atualização do preview/deploy está implícita.


## Encerramento
- Gate de launch literal executado: `cargo +1.98.0 run --release --manifest-path <worktree>/Cargo.toml -p edger-orchestrator --bin edger`, fixture isolada e curls de `/health` e `/bench-app@1.0.0` com exit 0/corpo esperado. A primeira tentativa foi impedida pelo bind do sandbox; a seguinte expirou durante compilação com budget de 30s. A execução final fora do sandbox completou com budget de 120s. Servidor próprio encerrado e fixture removida; não é benchmark adicional. O log de encerramento via SIGTERM do grupo registrou `process.drain.timed_out`; este gate não prova shutdown graceful.
- Preview preexistente em 19081 preservado, health 200; ele não foi atualizado para este código.
- Cache exclusivo `/tmp/edger-version-release-target-20261001` removido depois de todos os relatórios, sem processos usando o alvo. Os dois binários realmente medidos foram preservados em `/tmp/edger-version-bench-binaries-20261001/`, com hashes iguais aos da proveniência. O build posterior do gate cargo run não substitui o artefato medido preservado. Limpeza de `.pyc` regenerável próprio concluída.
- Gates de planejamento passaram com 0 RED e 46 WARN históricos (por exemplo campos Origin ausentes em epics antigos). Esses avisos de planejamento não são warnings de compilação Rust e não foram apresentados como lint completamente sem avisos.
- Pi/Qwen `keys-qwen` implementou a readmissão; pi/Qwen `cpanel-qwen` preparou/corrigiu e executou os harnesses; Sonnet `review-cpanel-claude` revisou o pool e auditou os resultados/sondas de validação. Orquestrador integrou a guarda de prewarm, corrigiu interpretações, preservou evidências, executou gates de planejamento/launch e limpou caches próprios.
- Otimizações indicadas mas fora deste pedido: fallback semver eager no default e scan de habilitação por candidato de plugin. Ao acessar rotativamente mais de 32 grupos, a evicção continua podendo causar cold start; agora a versão volta a ser atendida em vez de ficar bloqueada.

Limpeza final adicional: cache com tamanho aparente 1.391 GiB; delta de espaço livre observado 1.328 GiB (não desconta atividade concorrente de outros projetos). Volume de dados com 41.1 GiB livres no checkpoint final. A limpeza inicial já tinha observado cerca de 18 GiB liberados.


## Preparação da publicação — 2026-10-01
Operador autorizou commit, MR, merge e limpeza local após o fechamento das medições. A branch `fix/lru-readmission` foi criada de `main` (`276b507`) com somente a fatia LRU, seus testes/harnesses e evidências. A base rastreada do checkout usado nas medições (`ac5a325`) tem árvore idêntica a `276b507`, mas havia também alterações locais anteriores de console/admin não incluídas neste MR; a proveniência registra `git_dirty=true`. Os hashes dos módulos de pool/index/router usados para esta fatia não mudaram. Os números são evidência histórica local daquele binário preservado, não um benchmark do binário produzido pela CI deste MR. Gate e revisão da branch limpa verificam a integração exata.

Follow-ups de otimização e teste de prewarm registrados no ai-memory, scope explícito `workspace=djalmajr`, `project=edger`, página `follow-ups/version-routing-performance.md`. Tag e deploy não fazem parte desta autorização.

Os dumps históricos completos de lint/path foram retirados deste MR porque incluem documentos da entrega anterior de console. O CSV publicado foi normalizado de CRLF para LF, com igualdade das linhas parseadas e ambos os hashes no manifesto; os resultados originais permanecem preservados.

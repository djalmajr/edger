# EDG-61 — diagnóstico histórico de memória da rc.30

Parecer original de 2026-11-05; curadoria de publicação em 2026-11-09.
O código investigado era o da `v0.3.2-rc.30`, revisão
`1b4cc9635de26897cd49565536f02414bfaf9fd1`. Este registro não contém
medições atuais nem afirma inspeção direta dos processos em produção.
O original local foi preservado privadamente; o Cinzel continua sendo a
fonte canônica de planejamento e do histórico operacional.

O parecer original relacionou a retenção observada ao piso de processos
por versão habilitada. A inspeção estática dessa revisão confirma que
`prewarm_min_process_workers` percorre as versões indexadas e seleciona
`enabled`, `minProcesses > 0` e backend de processo, sem filtrar apenas a
default. O pool também protege o piso no TTL. Assim, repetir um piso em
várias versões pode manter vários runtimes aquecidos; isso oferece uma
explicação de retenção, mas não demonstra nem exclui vazamento de memória.

Um runtime persistente conserva V8/Deno, módulos e objetos inicializados.
RSS inclui memória além do heap JS; `max-old-space-size` limita old space,
não o RSS total. Separar custos do runtime, da aplicação e de memória
compartilhada exige série temporal e medidas como PSS/Private. Uma amostra
de RSS ou uma janela sem logs não basta para essa atribuição.

`minProcesses: 0` com TTL positivo permite criar processos sob demanda e
reutilizá-los até o prazo ocioso. É diferente de `ttl: 0`, que usa execução
efêmera. O custo da primeira requisição depende de spawn, import, bundle,
inicialização e primeira execução da aplicação; o parecer não estabeleceu
uma medida de cold start transferível para outras aplicações ou versões.
O cron segue a versão default: guardar um manifesto antigo com cron não
cria, por si só, um agendamento independente dessa versão.

A proposta histórica era separar retenção de arquivos de versões da
decisão de manter processos quentes. Aquecer apenas a default e tornar
rollback quente uma opção explícita era uma proposta de política, não um
comportamento entregue por este parecer. Alterá-la exige coordenar
prewarm, TTL, reposição e transições de versão, além de medir o efeito em
pin, rollback e latência; nenhuma ação operacional decorre desta nota.

A ressalva da rc.30 sobre desabilitar uma versão sem liberar seus processos
foi superada pelo EDG-94, integrado em `21eb6e3` e incluído na rc.32.
Após persistir o disable, o controle libera os processos pelo drain limitado
do recycle; se a persistência falhar, não os fecha. O pool consulta o
enabled atual antes de proteger ou repor o piso, inclusive em prewarm
atrasado. Isso não equivale a uma política de aquecimento apenas da default.
Essa atualização foi verificada no código de `origin/main` em
`fe44308ae86abe3f92f14db7f411befc22bd7058`, sem execução de runtime ou
nova coleta de métricas.

Referências de código: na revisão rc.30, `prewarm_min_process_workers` em
`crates/edger-orchestrator/src/deploy.rs`, `reserve_ttl_termination` e
`maybe_replenish_min_processes` em `crates/edger-worker/src/pool.rs`, e o
contrato de cron no `CLAUDE.md` dessa revisão. Para o EDG-94, consultar
`worker_mutation` em `crates/edger-orchestrator/src/admin_api.rs`, o pool
atual e [Worker enable, disable and recycle](../../../../charts/edger/README.md#worker-enable-disable-and-recycle).

# Scaling: L1 pool interno, L2 HPA, L3 fora de escopo

**Status:** operacional para Story 18.E (2026-07-03). Os números de baseline
devem ser preenchidos a partir do harness executado fora do sandbox.

EdgeR separa escala em níveis. Essa separação evita tratar réplicas Kubernetes
como substituto para concorrência dentro de um worker quente.

## L1: pool interno por worker

L1 é configurado no manifesto de cada worker, não no chart global:

- `maxProcesses`: teto de processos persistentes para o mesmo worker dentro de
  uma réplica do EdgeR.
- `minProcesses`: piso mantido do grupo de processos do worker, não um prewarm
  de uma vez: o pool pré-cria as instâncias quando o worker entra e mantém ao
  menos `minProcesses` vivas. Expirações de `ttl` que derrubariam o grupo
  abaixo do piso mantêm a instância `Idle` com o timer rearmado (a decisão é
  atômica com a contagem de instâncias vivas), e qualquer remoção que deixe o
  grupo abaixo do piso agenda uma tentativa de reabastecimento em segundo
  plano (uma por remoção, sem loop de retry; a tentativa que falha no spawn
  nunca agenda sucessora). A tentativa revalida a GERAÇÃO do grupo no
  momento de executar: se o grupo foi evictado pelo LRU, fechado ou saiu do
  cache (inclusive esvaziado pela própria remoção), ela não faz nada — a
  identidade só volta no caminho sob demanda (próxima requisição pré-cria
  uma geração nova com o piso). `ttl: 0` mantém a semântica efêmera (apenas
  prewarm na entrada, sem reabastecimento); grupos evictados e shutdown
  também não reabastecem. O piso continua valendo do teto `maxProcesses`, do
  LRU e das métricas; a evicção LRU segue terminando o grupo inteiro. O
  install, o promote e o enable pré-aquecem o piso em segundo plano (a
  resposta dessas rotas não espera o prewarm e informa `prewarm:
  "scheduled" | "not_configured"`); o startup e o rescan com `dryRun: false`
  também pré-aquecem, mas esperam o prewarm (o boot e a resposta do rescan).
- Aquecimento (`warmup`): campo opcional do manifesto que, logo depois do
  spawn feito pelo prewarm ou pela reposição do piso — e antes de o processo
  ficar `Idle` — envia **uma** requisição sintética `GET` (sempre `GET`, não
  há campo de método) ao próprio processo novo, para o primeiro usuário
  encontrar o código já executado uma vez. O formato é um objeto com `path`
  (obrigatório, pathname absoluto iniciado por `/`) e `timeout` (opcional,
  mesma duração do `timeout` do `healthCheck`), com default de **10 s**
  quando ausente ou não parseável. O limite efetivo do aquecimento é o menor
  entre `warmup.timeout` e o `timeout` do worker, porque o processo inicia com
  o timeout de quadro do worker; ao estourar, o aquecimento conta como falha
  (o processo é terminado, sem reposição automática):

  ```yaml
  minProcesses: 2
  warmup:
    path: /
    timeout: 5s
  ```

  O aquecimento **não conta**: não incrementa o `request_count` da instância
  (o orçamento de `maxRequests` inteiro fica para os usuários), não passa
  pelo `Supervisor` (não arma TTL), não entra nos contadores de requisições
  nem nas métricas e não registra sucesso nem falha no circuit breaker.
  Status 2xx/3xx e qualquer outro status deixam o processo `Idle` (o socket
  está íntegro; status fora de 2xx/3xx só gera `warn` no log). Falha de
  dispatch do aquecimento (timeout, crash ou erro de protocolo) só gera log;
  o processo é terminado e a instância é removida **sem reposição
  automática** (evita o laço spawn → falha no aquecimento → reposição) e a
  falha nunca derruba o boot, o rescan ou o install. Uma requisição de
  usuário que chega durante o aquecimento espera pelo dispatch lock e é
  atendida depois, no mesmo processo. O aquecimento só roda em processos
  criados pelo prewarm ou pela reposição: nunca no caminho sob demanda, em
  workers efêmeros (`ttl: 0`) nem em instâncias já existentes.
- `concurrency`: alias operacional normalizado junto com `maxProcesses`.
- `queueLimit`: quantidade máxima de requests persistentes esperando quando
  todos os processos daquele worker estão ocupados.
- `queueTimeout`: tempo máximo de espera antes de devolver erro tipado de
  capacidade.

Esse nível aumenta concorrência intra-réplica. Se um worker tem
`maxProcesses: 4`, uma réplica pode manter até quatro processos isolados para
aquele worker e reduzir head-of-line blocking de requests concorrentes.

O custo é memória e CPU por processo. Cada processo aplica os limites normais
do worker (`lowMemory`, heap cap, `ttl`, `idleTimeout`, `maxRequests`) e aparece
nas métricas do pool. O operador deve subir `maxProcesses` apenas para workers
que realmente têm concorrência ou streams longos.

## L2: HPA do chart

L2 é escala de réplicas do EdgeR via Kubernetes HPA. O chart em `charts/edger`
renderiza `charts/edger/templates/hpa.yaml` quando `hpa.enabled` está ativo e
usa os valores reais:

- `hpa.enabled`
- `hpa.minReplicas`
- `hpa.maxReplicas`
- `hpa.targetCPUUtilizationPercentage`

O `questions.yaml` expõe esses campos no grupo `Scaling`, junto com
`replicaCount` para o caso em que HPA está desligado. O HPA aumenta ou reduz pods
inteiros do EdgeR conforme CPU média da Deployment.

HPA não resolve sozinho head-of-line blocking de um worker quente quando cada
réplica continua com `maxProcesses: 1`. O roteador externo pode distribuir
requests entre réplicas, mas cada réplica ainda serializa aquele worker em um
único processo. Para workloads concorrentes, configure L1 primeiro e use L2
para ampliar capacidade total.

## Combinação recomendada

Use L1 para ajustar concorrência por worker e L2 para multiplicar a capacidade
da réplica:

1. Identifique workers com saturação, fila, `wait_ms_p95`, rejeições ou streams
   longos.
2. Aumente `maxProcesses` no manifesto desses workers até reduzir espera sem
   exceder orçamento de memória por pod.
3. Mantenha `queueLimit` limitado para preservar backpressure explícito.
4. Ative HPA quando a carga agregada justificar mais pods do EdgeR.
5. Ajuste `resources.requests.cpu` porque o HPA por CPU usa esse request como
   denominador.

Exemplo: com `maxProcesses: 4` e HPA entre 2 e 6 réplicas, o worker pode ter até
8 processos persistentes quando há 2 pods e até 24 processos quando o HPA chega
a 6 pods. A capacidade real depende do roteamento externo, CPU, memória e perfil
do worker.

## L3: Knative/FaaS fora de escopo

L3, como Knative, FaaS ou autoscaling por instância efêmera de worker, está fora
do escopo do Epic 18. Não construir L3 neste épico. O runtime permanece
stateless e HPA-ready; qualquer modelo FaaS deve ser tratado como arquitetura
separada, com nova story e critérios próprios.

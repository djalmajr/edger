# Deployment: EdgeR em Kubernetes

**Status:** chart de referência criado e validado em cluster real em 2026-07-03
pela Story 17.F (`planning/edger/epics/17-edger-minimalista/06-deployment-k8s.md`).

O chart vive em `charts/edger/` e segue o formato Rancher Apps & Marketplace com
`questions.yaml`. Usa Deployment, Service, ConfigMap, Secrets opcionais (inclusive
o cofre de segredos de apps, `appSecrets`), Ingress e HPA opcionais. Com `userWorkers.persistence.enabled`, os workers e o SQLite
do plano de controle ficam no PVC RWO; o Deployment usa `Recreate` e uma
réplica. Sem persistência, o estado local não sobrevive à substituição do Pod.

## Auth

- `rootKey.value`: gera um Secret do chart.
- `rootKey.existingSecret`: referencia um Secret já existente.
- Ambos montam a chave em `/var/run/secrets/edger-root/root-key` e configuram
  `EDGER_ROOT_KEY_FILE` para esse caminho. A rotação acontece atualizando o
  Secret; o runtime relê o arquivo sem restart.
- `consoleAuth.rootPasswordSecret.name` referencia um Secret existente e
  opcional para a senha inicial de `root`; `key` escolhe a entrada do Secret
  (default `password`). O chart monta o arquivo read-only e define apenas
  `EDGER_ROOT_PASSWORD_FILE`, nunca o valor da senha em values ou ConfigMap.
  A senha só semeia `root` quando ainda não existe usuário `root`; após o
  primeiro boot, alterar o Secret não substitui a senha já armazenada.
  Usuários e sessões compartilham o SQLite de API keys (`EDGER_API_KEYS_DB`),
  portanto exigem persistência do volume para sobreviver ao ciclo do Pod.
  Sem esse Secret, a entrada por token continua disponível.
- O papel admin e a senha temporária são estado do banco/da API de admin,
  **não** valor do chart: o chart segue expondo apenas
  `consoleAuth.rootPasswordSecret.name/key` (mecanismo de seed; alterar o
  Secret depois não reseta a senha do `root` já existente). O usuário `root`
  é único e imutável (papel `admin`, `isRoot`); só root cria, promove ou
  rebaixa o papel admin. Uma sessão de console com papel admin (credencial
  `ses-`) gerencia os operadores cobertos — permissões, namespaces e workers
  do alvo todos dentro dos próprios grants do ator (sem subunção de glob; `*`
  cobre a dimensão de escopo); a linha do root e os demais admins aparecem
  sem ações. A chave `egk_` com papel admin não ganha gestão de
  usuários/sessões. O admin pode criar/editar/desabilitar/excluir operadores
  cobertos, resetar a senha e revogar sessões; pode revogar as próprias
  sessões exceto a corrente (só o logout termina a corrente). Rebaixamento,
  desativação e redução de grants revogam as sessões do alvo na mesma
  escrita. Criação/reset por outro usuário gera senha temporária: até o
  usuário escolher uma nova diferente, toda rota de admin responde
  `403 PASSWORD_CHANGE_REQUIRED` exceto `GET /api/admin/session`,
  `POST /api/admin/logout` e `POST /api/admin/me/password`; a troca
  bem-sucedida rotaciona as sessões e devolve o token novo. O cPanel mostra a
  tela de troca (sem fechar) antes do Shell em todos os caminhos de entrada
  (senha, token colado, sessão restaurada).
- OIDC genérico está implementado e fica opt-in via `oidc.enabled`, `oidc.issuer`,
  `oidc.audience`, `oidc.rolesClaim` e `oidc.requiredRole`, renderizados como
  `EDGER_OIDC_*`.

## Roteamento: tenant e split A/B

O chart expõe duas flags independentes, desligadas por padrão, que habilitam
as duas metades da política de roteamento por app:

- `tenantRouting.enabled` (bool, `false`): allowlist de tenant. Com a flag
  ligada, o EdgeR resolve o hostname do request na Consumer API do Tenancit
  e só serve apps cuja política liste o slug de tenant identificado. A
  renderização falha sem `tenantRouting.tenancit.identifyUrl` (URL HTTPS
  exata do endpoint `/v1/identify` no cluster real, sem query/fragment/
  userinfo; HTTP só em loopback) e sem a referência do Secret existente
  (`tokenSecret.name` + `tokenSecret.key`) com o token do API client
  `tenant:identify`.
- `weightedRouting.enabled` (bool, `false`): split A/B de versões. Com a flag
  ligada, requests sem versão explícita seguem os pesos da política do app;
  pode ser ligada sozinha.

O ConfigMap sempre passa `EDGER_TENANT_ROUTING_ENABLED` e
`EDGER_WEIGHTED_ROUTING_ENABLED` ("true"/"false") e passa
`EDGER_TENANCIT_IDENTIFY_URL` apenas com tenant ligado. O token do Tenancit
nunca entra no ConfigMap, nos values da release ou nos logs: o Secret
existente é montado read-only em caminho fixo
(`/var/run/secrets/edger-tenancit/token`) e o Deployment define apenas
`EDGER_TENANCIT_TOKEN_FILE` com esse path. Com as flags off, a renderização
padrão não referencia Tenancit em env, volume ou Secret.
O EdgeR lê esse arquivo no startup; após rotacionar o Secret, reinicie o Pod
para carregar o token novo.

Limites (sem afirmação de produção):

- O domínio identificado **não autentica o usuário**: é contexto de domínio
  que decide quais tenants alcançam o app; a autenticação/autorização dos
  dados continua sendo responsabilidade de cada worker.
- A política de roteamento é local ao processo/PVC (índice em memória,
  persistida no PVC de workers); o chart continua single-replica, então não
  há sincronização multi-réplica a garantir — mas também não há o que
  distribuir ainda.
- Não assumir que `traffic`/allowlist já está publicado: sem política, vale o
  roteamento atual.

## SSE, WASM e banco gerenciado

Esta seção descreve o que muda para quem opera, a partir do que o chart expõe
hoje. Os recursos abaixo estão implementados neste branch (EDG-52 a EDG-56), sem
commit e sem release; nada aqui afirma que algum ambiente já os executa.

**SSE mantido pelo host** ([sse.md](sse.md)). Três variáveis, lidas uma vez no
boot, sem teto superior; valor inválido gera `warn` e usa o padrão:

- `EDGER_SSE_MAX_CONNECTIONS` (padrão `1024`, total no processo; `0` mantém o
  broker registrado mas desligado, e toda assinatura autorizada dá
  `503 SSE_UNAVAILABLE`).
- `EDGER_SSE_MAX_CONNECTIONS_PER_APP` (padrão `256`, mínimo `1`; teto sobre o
  `sse.maxConnections` do manifest).
- `EDGER_SSE_HEARTBEAT_MS` (padrão `15000`, mínimo `1000`; também é a
  granularidade do TTL das assinaturas).

O chart **não tem valor dedicado** para elas: hoje só chegam ao Pod por
`extraEnv` ou `extraEnvFrom`. Para o operador, o que importa: o broker é por
processo e o chart é de uma réplica, então não há fan-out entre pods; um cliente
que parou de ler segura a vaga até o TCP acabar; e desabilitar ou apagar um worker
não derruba assinaturas abertas (só o TTL delas). Métricas em `/metrics`:
`edger_sse_connections`, `edger_sse_events_published_total`,
`edger_sse_disconnects_total` e `edger_sse_rejected_total`.

**Banco SQLD gerenciado** ([sqld-managed.md](sqld-managed.md)). O EdgeR gerencia
namespaces de um `sqld` **externo**; o chart não inclui `sqld` e não expõe as sete
variáveis `EDGER_SQLD_ADMIN_URL`, `EDGER_SQLD_ADMIN_KEY_FILE`,
`EDGER_SQLD_USER_URL`, `EDGER_SQLD_REGISTRY_DB`, `EDGER_SQLD_SIGNER_KEY_FILE`,
`EDGER_SQLD_MANAGED_BUDGET_BYTES` e `EDGER_SQLD_BACKUP_DIR`. Todas sem padrão e
todas ou nenhuma: com parte delas, o `edger` não sobe. Além delas, o recurso
exige o cofre de segredos (`appSecrets.enabled` com `appSecrets.keySecret.*`).
Como o chart usa `readOnlyRootFilesystem` e não monta volumes para o ledger, as
chaves e os backups (diretórios `0700` do usuário do processo), usar o recurso em
Kubernetes pelo chart não é suportado hoje; é uma lacuna a fechar.
Os manifestos de `deploy/production/sqld.yaml` mantêm o admin do `sqld` em
`127.0.0.1:9090` dentro do pod dele, inalcançável pelo `edger`.

**Segredos e egress já expostos pelo chart.** `appSecrets.*` vira
`EDGER_APP_SECRETS_DB` e `EDGER_SECRETS_KEY_FILE` (a chave vem de um Secret
existente, nunca do chart) e `egress.blockCidrs` vira `EDGER_EGRESS_BLOCK_CIDRS`
(o IP do nó entra sozinho em `EDGER_NODE_IP`). O WASM usa os mesmos recursos:
segredos por execução e o guard de egress pelo import `edger_http_v1.fetch`
([wasm-abi.md](wasm-abi.md)).

**Encerramento do Pod.** A parada do `edger` (SIGTERM) drena as conexões HTTP
por no máximo `EDGER_HTTP_DRAIN_MS` (padrão `10000`, contado a partir do sinal;
`0` desliga o limite e a espera volta a ser sem fim; valor inválido gera `warn` e
vale o padrão). Ao expirar, as conexões que sobraram morrem com o processo, depois
de um `warn`; o cron e o pool encerram normalmente e a saída é com sucesso. Vale
para todas as conexões HTTP, não só o SSE. Os prazos padrão somam a drenagem
(10 s), o cron (7 s: 5 de graça, 1 de abort e 1 de margem) e o pool (até 15 s), ou
seja 32 s. Essa soma **não é um teto garantido**: depois de abortar o supervisor
do cron, o processo espera ele terminar sem prazo, e uma passada de reconciliação
em andamento não é interrompida pelo abort. O chart **não define**
`terminationGracePeriodSeconds`, então vale o padrão de 30 s do Kubernetes,
**menor** que essa soma: quando os prazos se esgotam, o kubelet envia SIGKILL antes
de o `edger` terminar sozinho. Esta seção descreve a conta a partir
do código; não houve teste em cluster. O chart não expõe a variável; ela só chega
ao Pod por `extraEnv`.

**WASM efêmero.** Um `kind: wasm` sem `ttl` recompila o módulo a cada pedido
(cerca de 9 s no perfil `dev` com um módulo Go de 6,6 MB; release não medido).
Declare `ttl` nos guests Go.

## Operação

- `/livez` é a probe de liveness.
- `/ready` é a probe de readiness real.
- `/` redireciona para `/cpanel/`; o Ingress usa `ingress.path` configurável,
  com default `/`.
- O API Gateway externo fica na frente do Service/Ingress quando necessário; o
  chart não embute gateway stateful nem armazenamento.

## Escala: L1 por worker e L2 por HPA

O chart expõe apenas escala L2 de réplicas do EdgeR. `charts/edger/values.yaml`
define `replicaCount` e `hpa.enabled`, `hpa.minReplicas`,
`hpa.maxReplicas`, `hpa.targetCPUUtilizationPercentage`; o
`charts/edger/questions.yaml` expõe esses campos no grupo `Scaling`; e
`charts/edger/templates/hpa.yaml` renderiza um
`HorizontalPodAutoscaler` `autoscaling/v2` apontando para a Deployment.

`maxProcesses`, `minProcesses`, `concurrency`, `queueLimit` e `queueTimeout`
são configuração L1 por worker no manifesto, não env global do pod. Por isso o
chart não cria um valor global de pool: um único knob de Deployment misturaria
workloads diferentes e esconderia o orçamento de memória por processo.

`minProcesses` é um piso mantido do pool (não prewarm de uma vez): expirações
de `ttl` que derrubariam o grupo abaixo dele mantêm a instância `Idle` (timer
rearmado) e qualquer remoção que deixe o grupo abaixo do piso agenda uma
tentativa de reabastecimento em segundo plano, que revalida a geração do
grupo ao executar (se o grupo saiu do cache — evicção, fechamento ou
esvaziamento — a tentativa não faz nada e a identidade volta pela próxima
requisição); `ttl: 0` mantém a semântica efêmera (apenas prewarm na
entrada). O piso conta do teto `maxProcesses` e do LRU, e a evicção
LRU segue terminando o grupo inteiro.

Use L1 para remover head-of-line blocking de um worker quente dentro da réplica;
use L2 para multiplicar a capacidade total com mais pods. HPA sozinho não
resolve o caso em que cada réplica mantém `maxProcesses: 1` e o mesmo worker
serializa requests dentro de cada pod. A operação completa está descrita em
`planning/edger/docs/scaling.md`.

## Validação em cluster (2026-07-03)

- Cluster: K3s v1.35.5 single-node (Hetzner , x86_64, Ubuntu 24.04); helm v4.2.2.
- Imagem: cross-compile local + montagem docker buildx amd64 (216MB) — nota: rustc SEGFAULTa sob emulação x86 no Docker Desktop/Apple Silicon; zigbuild é a rota para builds amd64 locais. Import direto no containerd (sem registry, validação apenas).
- `helm install` → sucesso; pod 1/1 Running em 11s, sem restart.
- Provas in-cluster (port-forward svc :3000): `/health` 200, `/ready` 200, `/` → 307 `/cpanel/`, cPanel servido do pod, admin sem key 401, admin com a chave do Secret-arquivo 200.
- Rotação de root key sem restart: chave nova aceita após ~75s (re-projeção do kubelet), chave antiga 401, restartCount=0.
- Limpeza completa: helm uninstall + namespace e imagem removidos.
- Pendência que resta: exposição via Cloudflare Tunnel/ingress e deploy contínuo (kaniko/registry, runbook 08 do infra) ficam para quando o edger for a produção de fato.

## Upgrade da plataforma para o 0.3.2-rc.30

Procedimento para o upgrade da release `edger` (namespace
`edger`) para o `0.3.2-rc.30` — a primeira versão com o papel admin da
console e a senha temporária (EDG-45/46/47 no backend, EDG-48/49 no cPanel;
EDG-50 documenta o contrato). Execute as etapas na ordem abaixo e registre
separadamente a versão, a revisão e o resultado das verificações da operação.

1. **Backup consistente do `api-keys.db` antes do upgrade.** O arquivo fica no
   PVC local `edger-user-workers` e guarda as chaves `egk_`, os usuários da
   console (`console_users`) e as sessões (`console_sessions`). A migração do
   rc.30 é aditiva e idempotente (`ALTER TABLE console_users ADD COLUMN
   must_change_password INTEGER NOT NULL DEFAULT 0`, executada só com a
   coluna ausente; usuários existentes mantêm `0`, sem recriar tabela), mas o
   backup é a âncora do rollback: cópia consistente via export de estado
   (`VACUUM INTO`) ou backup online do SQLite; uma cópia bruta do arquivo só
   é consistente com o EdgeR parado.
2. **Upgrade do chart para o `0.3.2-rc.30`** (a imagem é fixada por digest no
   chart; manter `--history-max 5` para o `helm rollback`).
3. **Promoção do `djalmajr` para admin, sem reset de senha.** Localizar a
   linha do `djalmajr` em `GET /api/admin/users` (com a root key) e aplicar
   `PATCH /api/admin/users/{id}` com exatamente `{"role":"admin"}` — um único
   campo; o SQL da mutação não toca `password_hash`, `password_changed_at`
   nem `must_change_password`, então a senha e o estado de senha temporária
   do usuário não mudam. A promoção **não** revoga as sessões do alvo (ao
   contrário do rebaixamento): o token `ses-` existente passa a reportar
   `role: "admin"` no `GET /api/admin/session` a partir do próximo request.
   Se a linha não existir ou `isRoot` for `true`, parar e reabrir o escopo do
   EDG-50 (a linha do root é imutável, `403 USER_IMMUTABLE`).

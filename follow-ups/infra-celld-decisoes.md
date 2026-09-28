# Infra e `celld`: decisões e alternativas para validação

Demanda: alinhar infra com o planner, a começar pelo `denoland/celld` na VPS
e no labdev (pedido do operador, 2026-09-25). Não há issue no Linear; este
arquivo é o registro. A resposta ao planner está em
`planner/.herdr-agents/w7/reply-from-edger-2026-09-25-infra.md`.

## C1. `celld` como runtime separado, em piloto
- **Decisão:**
  - O `celld` roda ao lado do EdgeR, como runtime próprio para apps no
    formato Workers (o `build:cf` do Planner). O EdgeR não muda.
  - A ordem é: spike local do planner, depois piloto na VPS, depois labdev
    com o ok do operador.
- **Por quê:**
  - O `celld` roda um app por fleet (um bucket), com estado replicado. O
    EdgeR é multi-app, sem estado, com control plane. Os dois se
    complementam.
  - O `celld` é 0.x (0.5.1 em 2026-09-19), com formato de fleet que ainda
    muda, e mixed versions não convivem.
- **Alternativas:**
  - O EdgeR incorporar DO, lease e LTX: seria refazer o `celld`.
  - O EdgeR orquestrar o `celld` (`kind: wrangler`): acopla o EdgeR a um
    projeto 0.x. Reavaliar depois do piloto.
- **Reverter:** baixo. Ainda nada foi implantado.
- **Onde:** um chart ou manifests novos, depois do spike.
- **Status:** aprovada pelo operador em 2026-09-25 ("Agora, como piloto, para
  a vps, r2, para labdev, minio"). A infra (bucket, Secret e manifests) sai
  em paralelo ao spike, validada com um app de exemplo do próprio `celld`.

## C2. Bucket do piloto na VPS: R2 direto
- **Decisão:** usar Cloudflare R2 pelo endpoint S3
  (`<conta>.r2.cloudflarestorage.com`), sem proxy, para o piloto na VPS.
- **Por quê:**
  - A VPS não tem MinIO hoje: `minio-api.djalmajr.dev` não resolve, e não
    há Service `minio` no k3s (conferido em 2026-09-25).
  - Com um nó só, o `celld` depende do bucket para a durabilidade. Um MinIO
    no mesmo nó não protege contra a perda do nó.
  - O endpoint S3 do R2 é direto, então o SigV4 funciona, ao contrário do
    gotcha do proxy.
- **Alternativas:**
  - MinIO no nó: mais rápido, mas a durabilidade fica sendo o snapshot
    diário do disco.
  - Um segundo nó pelo `headscale` para formar o ensemble: melhora a
    latência, mas é mais operação.
- **Reverter:** baixo.
- **Onde:** um Secret do bucket no namespace do piloto.
- **Status:**
  - Bucket `celld-planner` criado em 2026-09-25 com `wrangler` 4.141.0
    (atualizado de 4.45.0), localização WEUR, conta `1427e570...b3d`.
  - As credenciais S3 dependem do token de API do R2 (C5), que o operador
    cria no dashboard.

## C3. Labdev só depois da VPS, com bucket do MinIO existente
- **Decisão:** no labdev, usar o MinIO do namespace `minio`, com bucket e
  credenciais de quem o administra. Um StatefulSet com duas ou mais réplicas
  forma o ensemble.
- **Por quê:** o cluster é compartilhado. O piloto na VPS valida antes.
- **Alternativas:** subir um MinIO próprio no labdev: duplica storage num
  ambiente compartilhado.
- **Reverter:** baixo.
- **Onde:** labdev, com o ok do operador.
- **Status:** bloqueada (ok do operador e bucket do administrador).

## C4. Backup do estado do EdgeR em bucket na 0.3.3
- **Decisão:** a parte "b" que vale para o EdgeR é guardar o próprio
  estado (workers, `.edger-defaults/` e `api-keys.db`) em bucket, como
  export/import.
- **Por quê:** já estava planejado (item 8 da lista da 0.3.2) e liga com a
  D16 (overlay em `emptyDir`).
- **Alternativas:** Litestream só para o `api-keys.db`: não cobre os
  workers.
- **Reverter:** baixo.
- **Onde:** EdgeR 0.3.3.
- **Status:** na fila.

## C5. Token do R2: criado pelo operador, restrito ao bucket e ao IP da VPS
- **Decisão:**
  - Nome do token: `celld-planner-vps`.
  - Permissão: **Object Read & Write** só no bucket `celld-planner`.
  - Filtro de IP do cliente: `167.235.206.217`.
  - TTL: sem expiração, com rotação manual.
  - As credenciais vão direto para o Secret `celld-r2` no namespace `celld`
    da VPS (chaves `AWS_ACCESS_KEY_ID` e `AWS_SECRET_ACCESS_KEY`), sem passar
    pelo chat.
  - O `celld deploy` roda a partir da VPS (nó ou Job no cluster).
- **Por quê:**
  - O token do wrangler (variável `CLOUDFLARE_API_TOKEN`) cria buckets, mas
    recebe 403 em `/user/tokens` e `/accounts/{id}/tokens`, então não
    consegue emitir credenciais.
  - O README diz que quem tem as credenciais do bucket é administrador da
    fleet. Escopo mínimo e filtro de IP limitam o estrago de um vazamento.
- **Alternativas:**
  - Dar ao token do wrangler a permissão "API Tokens: Edit" para eu criar o
    token: amplia um token que já fica no ambiente local.
  - Token sem filtro de IP: permite `celld deploy` do Mac, mas vale de
    qualquer lugar.
- **Reverter:** baixo. O token pode ser revogado e recriado.
- **Onde:** dashboard da Cloudflare (R2 → Manage API tokens) e Secret no k3s.
- **Nota (https://celld.dev/docs/services/r2/):**
  - Os bindings R2 do app gravam no **próprio bucket da fleet**, sob
    `r2/<bucket_name>/`, sem segundo bucket nem segunda credencial.
  - O token acima cobre o estado da fleet e o R2 do app. Uploads acima de
    8 MiB viram multipart, que o Object Read & Write inclui.
- **Status:** bloqueada (o operador cria o token).

## C6. Manifests do piloto no repositório `infra`, StatefulSet de uma réplica
- **Decisão:**
  - Os manifests ficam em `infra/celld/` (repositório privado, branch
    `feat/celld-pilot`):
    - StatefulSet `celld`, uma réplica, imagem `v0.5.1` fixada no digest;
    - `CELLD_WATCH` e o asset cache num PVC `local-path` de 5Gi;
    - `startupProbe`, `readinessProbe` e `livenessProbe` em
      `/.well-known/celld/health`;
    - `terminationGracePeriodSeconds: 90`;
    - `--advertise` pelo DNS do pod no headless Service;
    - NetworkPolicy com a porta 8081 só entre pods do `celld`, e a 8080 do
      namespace `celld` e do `traefik`.
  - Sem rota pública no piloto.
  - A imagem roda como root, com `allowPrivilegeEscalation: false` e
    capabilities removidas.
- **Por quê** (pesquisa `scout-qwen-20260925T184156`):
  - sem `CELLD_WATCH`, o estado vai para `/tmp/celld-<PID>`;
  - o health segura o primeiro 200 por até 120 s;
  - o SIGTERM faz handoff em até 40 s;
  - o listener interno tem API de operador sem autenticação;
  - o `--advertise` aceita DNS de pod.
- **Alternativas:**
  - Manifests no repositório público do EdgeR: expõe conta e bucket.
  - Chart Helm próprio: mais trabalho antes de validar o piloto.
  - Deployment em vez de StatefulSet: perde o DNS estável do pod.
- **Reverter:** baixo (`kubectl delete -k celld/`). O PVC e o bucket ficam.
- **Onde:** `infra/celld/`.
- **Status:**
  - Aplicada na VPS em 2026-09-25 (worktree `infra-celld`, branch
    `feat/celld-pilot`, commits `db1231b` e o do Job). O push e o PR para o
    `infra` esperam o ok do operador.
  - O pod `celld-0` está pronto e renova o lease no R2 em ~140 ms. A
    NetworkPolicy foi validada ao vivo: 8080 liberada só do namespace
    `celld`, e 8081 bloqueada para pods sem o label e para outros
    namespaces.

## C7. Uma fleet por prefixo do bucket; smoke só de assets
- **Decisão:**
  - A fleet de smoke usa `s3://celld-planner/smoke`, e o Planner usará
    `s3://celld-planner/planner`.
  - O smoke é um app só de assets (`celld-smoke`), publicado por um Job com
    a imagem oficial, que não tem `esbuild`.
- **Por quê:**
  - O `--bucket` aceita `NAME/PREFIX`, e cada fleet roda um app só. O
    prefixo separa os estados sem outro bucket nem outro token.
  - Um app só de assets não precisa de `esbuild`.
- **Alternativas:**
  - `examples/hello` (Worker): exige `esbuild` ou imagem derivada.
  - Um bucket por app: mais tokens e mais dashboard.
- **Reverter:** baixo.
- **Onde:** `infra/celld/smoke/`.
- **Status:** aplicada. Deploy `b6986c765fee93b3` em
  `s3://celld-planner/smoke`, e `http://celld:8080/` responde `celld-smoke ok`.

## C8. Pegadinhas do piloto registradas; ajustes que ficam para depois
- **Decisão:** registrar as pegadinhas no runbook
  (`djalmajr/infra` `runbooks/19-celld-pilot.md`) e deixar para o deploy do
  Planner:
  - `RUST_LOG=info,celld::ltx_repl=warn`, porque o `ship loop` loga toda
    segunda;
  - um Job com `esbuild` para publicar Worker.
- **Por quê:** observado ao vivo em 2026-09-25:
  - o `celld` só abre o listener público depois do primeiro deploy, e o
    `startupProbe` reiniciou o pod duas vezes antes disso;
  - volume de ConfigMap monta symlinks, e o `celld deploy` recusa;
  - o kube-router só libera um pod novo na NetworkPolicy depois de alguns
    segundos;
  - o `--dry-run=client` não pegou `envFrom` no nível do pod, e o
    `--dry-run=server --validate=strict` pegou.
- **Alternativas:** subir o `failureThreshold` do `startupProbe` para
  cobrir a fleet sem deploy. Esconderia um nó sem app, então preferi
  documentar: publique antes, ou aceite os restarts iniciais.
- **Reverter:** —
- **Onde:** runbook 19.
- **Status:** aplicada.

## C9. Fleet `planner` em espera; issues upstream como rascunho
- **Decisão:**
  - Não criar o Job de deploy da fleet `planner` enquanto o Planner não
    rodar no `celld`.
  - A infra do piloto (fleet `smoke`) fica no ar.
  - Os três problemas viram rascunhos de issue em
    `infra/celld/upstream-issues-draft.md`, publicados só com o ok do
    operador.
- **Por quê** (spike do planner,
  `planner/.herdr-agents/w7/reports/celld-spike-20260925T200449.md`):
  - A v0.5.1 publica um módulo único. Reempacotar o grafo do
    `@cloudflare/vite-plugin` quebra os ciclos do TanStack Start
    (`RouterContext is not a function`), e o SSR dá 500.
  - `node:crypto.scrypt` não existe, e o sign-up do Better Auth falha.
  - `import "node:stream";` sem `from` não registra o stub, e o Worker não
    carrega.
  - Assets, D1 (19 migrations), Durable Object com WebSocket e heap (~34 MB)
    funcionaram.
- **Alternativas:**
  - Subir mesmo assim: SSR e sign-up quebrados.
  - Esperar só pelo upstream: o planner vai testar um arquivo único gerado
    pelo Rollup (`inlineDynamicImports`) e `scrypt` em JS puro.
- **Reverter:** baixo.
- **Onde:** `infra/celld/` e o relatório do planner.
- **Status:** bloqueada (resultado do segundo experimento do planner, e ok
  do operador para publicar as issues).

## C10. Spike 2 aprovado; como o artefato do Planner chega ao cluster
- **Estado:**
  - O spike 2 do planner (`planner/.herdr-agents/w7/reports/celld-spike2-20260925T203418.md`)
    rodou SSR, auth e e2e no mesmo nível do build de produção.
  - Arquivo único do Rolldown (`output.codeSplitting: false`) com
    `no_bundle: true`, então o **Job não precisa de `esbuild`**.
  - `scrypt` em JS puro (`@noble/hashes`) é compatível com os hashes atuais.
  - A fleet `planner` segue em espera até o planner versionar o alvo `celld`
    no repositório dele, com o ok do operador.
- **Decisão para quando destravar** (proposta, não aplicada):
  - uma segunda StatefulSet `celld-planner`, com
    `CELLD_BUCKET=s3://celld-planner/planner`. A fleet `smoke` fica como
    canário.
  - O artefato (~6,9 MB, acima do limite de 1 MiB de ConfigMap) vai para o
    nó por `scp`. Um Job com `hostPath` somente leitura e `envFrom` do
    Secret roda `celld deploy` com a imagem oficial, e o segredo não sai do
    cluster.
  - As migrations entram por `celld d1 migrations apply` no mesmo Job.
- **Por quê:**
  - O deploy precisa sair do IP da VPS (filtro do token), e ConfigMap não
    comporta o artefato.
  - Um binário do `celld` instalado no nó exigiria ler o Secret fora do
    Kubernetes.
- **Alternativas:**
  - Subir o artefato no R2 (`celld r2 put`) e o Job baixar: mais uma peça
    no caminho.
  - Imagem OCI com o artefato: exige registry e build.
  - Trocar a fleet da StatefulSet atual para o prefixo `planner`: perde o
    canário.
- **Reverter:** baixo.
- **Onde:** `infra/celld/` (futuro `planner/`).
- **Status:** destravado pelo PR #24 do planner (`5557cfb`); o plano
  aplicado está na C11.

## C11. Fleet `planner`, fase 1: sem rota pública, `ENV=local`, banco vazio
- **Decisão:**
  - **Recursos novos no namespace `celld`**, com o rótulo
    `app.kubernetes.io/name: celld-planner`:
    - ConfigMap `celld-planner-env` (`CELLD_BUCKET=s3://celld-planner/planner`,
      o resto igual ao `celld-env`);
    - Services `celld-planner` (8080) e `celld-planner-peers` (headless,
      8081);
    - StatefulSet `celld-planner` (mesma imagem e digest, PVC `local-path`
      de 5Gi);
    - NetworkPolicy `celld-planner`: a 8081 só dos pods `celld-planner` e
      do Job de deploy, porque o `celld d1` fala com o nó pela porta
      interna; a 8080 só do namespace `celld`. Sem `traefik` nesta fase.
  - **Artefato:** `bun run build:celld` num clone descartável do planner em
    `5557cfb`, com as `vars` públicas do artefato (`ENV=local`,
    `APP_URL=http://localhost:3000`). Vai por `rsync` para
    `/var/lib/celld-artifacts/planner/current/` no nó, com o commit em
    `current.commit` (apagado antes do `rsync` e regravado depois).
  - **Job `celld-planner-deploy`:**
    - monta esse diretório por `hostPath` somente leitura;
    - copia para um `emptyDir`;
    - mescla em `vars` cada variável `CELLD_VAR_<NOME>` do Secret
      `celld-planner-vars` (com `awk`, porque a imagem não tem `jq`);
    - roda `celld deploy` e, com retentativas até o nó adotar a versão,
      `celld d1 migrations apply planner-dev`.
  - **Secret `celld-planner-vars`:** nesta fase só
    `CELLD_VAR_BETTER_AUTH_SECRET`. O operador o cria com um script que
    gera o valor na própria VPS (`openssl rand -hex 32`), então o valor não
    passa pelo Mac nem pelo agente.
  - **Acesso:** só por `kubectl port-forward` na porta 3000, porque com
    `ENV=local` o código OTP aparece na tela.
  - **Banco:** começa vazio.
- **Por quê:**
  - `ENV=local` dispensa as credenciais de e-mail e deixa validar SSR, auth
    e D1 persistido no R2 sem expor um cadastro aberto.
  - O Secret gerado na VPS segue a C10: o segredo não sai do cluster.
  - O `celld deploy` não tem flag de `var` (v0.5.1), então os segredos
    precisam estar no `wrangler.json` do diretório de deploy.
- **Alternativas:**
  - **Fase 2 já:** host público, `ENV=production`, e-mail e
    `AUTH_SIGNUP=invite`. Exige o token de e-mail num Secret, DNS e
    Ingress; fica para um ok próprio.
  - **Importar o dump do `sqld` agora:** mistura a validação da plataforma
    com a migração de dados; fica para decisão do operador.
- **Para a fase 2** (nota de 2026-09-25, a partir da revisão do planner,
  PR #27):
  - o `build:celld` só aceita `ENV` e `APP_URL` em `vars`;
  - o `deploy.sh` recusa sobrescrever chave presente, de propósito,
    porque só mescla segredos;
  - a troca de `ENV` e `APP_URL`, que são públicos, deve ser feita no
    estágio: o `stage-planner-artifact.sh` ganha `--var CHAVE=valor`
    restrito a essas duas chaves e reescreve o `wrangler.json` no Mac
    antes do `rsync`;
  - o artefato do planner segue igual para todos os ambientes.
- **Reverter:** baixo. `kubectl delete` dos recursos `celld-planner` e do
  Job; o PVC, o prefixo no bucket e o diretório no nó ficam até limpeza
  manual.
- **Onde:** `infra/celld/planner/` e o runbook 19 no ai-memory.
- **Status:** aplicada em 2026-09-26 (01:54 UTC), com o ok do operador:
  - artefato `planner@7fb13d0`, 19 migrations e SSR 200;
  - a primeira tentativa falhou porque o `celld` v0.5.1 reserva o prefixo
    `CELLD_VAR_` no ambiente. O `deploy.sh` passou a apagar essas
    variáveis antes de chamar o `celld` (`infra` `0c2854f`);
  - não houve exposição de segredo;
  - runbook 19 do `djalmajr/infra` no ai-memory atualizado.

## C12. Fleet `planner`, fase 2: host público `celld.djalmajr.dev`, `ENV=production`, e-mail e import do `sqld`
- **Decisão:**
  - **Host:** `celld.djalmajr.dev` — correção do operador (2026-09-26) sobre
    a proposta do planner (`planner-celld.djalmajr.dev`): somente esse
    domínio, com A record **sem proxy** (mesmo padrão de
    `planner.djalmajr.dev` e `edger.djalmajr.dev` neste nó). Com ele,
    `APP_URL=https://celld.djalmajr.dev` e
    `TRUSTED_ORIGINS=https://celld.djalmajr.dev` (origem exata, sem
    wildcard).
  - **Variáveis públicas (estágio do artefato):** `ENV=production` e
    `APP_URL=https://celld.djalmajr.dev` — as únicas que o `build:celld`
    aceita em `vars` (C11). O `--var` do estágio reescreve o
    `wrangler.json` no Mac antes do `rsync`. Sem `DATABASE_URL` no
    artefato: o banco é o binding D1 `DB`.
  - **Secret `celld-planner-vars` (o Job mescla no `wrangler.json`):**
    `AUTH_SIGNUP=invite`, `AUTH_IP_HEADERS=x-real-ip` (o Traefik do nó vê o
    IP real e escreve o header, como no deploy principal),
    `TRUSTED_ORIGINS` e o trio de e-mail Cloudflare
    (`CLOUDFLARE_ACCOUNT_ID`, `CLOUDFLARE_EMAIL_API_TOKEN` e
    `AUTH_EMAIL_FROM`).
  - **Segredos:** `BETTER_AUTH_SECRET` preserva o valor já gerado na VPS
    (C11; merge-patch, sem rotação). O valor das três variáveis de e-mail é
    fornecido **apenas pelo operador**, pelo procedimento seguro: nenhum
    valor de segredo entra neste registro, no repositório ou em mensagem.
  - **Import do `sqld`:** único import do dump de produção no D1 da fleet
    (`planner-dev`) com `celld d1 execute --file`, **após** o backup
    completo pré-import (que já existe) e a verificação de compatibilidade
    (origem com 18 migrações, destino com 19; a 19ª adiciona seis tabelas
    novas). A tabela `d1_migrations` **não** é importada — o estado de
    migração do destino (19) não é tocado. O `sqld` de origem não é
    alterado.
  - **Rota pública:** publicada só depois de testar, na versão implantada,
    que o OTP não aparece na tela (com `ENV=production` o código sai só por
    e-mail) e que o envio de e-mail funciona.
  - **Fora de escopo:** deploy da IA no EdgeR, publicação das issues
    upstream e ações no labdev.
- **Por quê:**
  - O ok do operador de 2026-09-26 ("passe também o ok para prosseguir com
    as próximas fases", com a correção do host) autoriza a fase 2 e o
    import; o contrato de configuração está em
    `planner/.herdr-agents/w7/to-edger-2026-09-26-celld-fase2.md`.
  - `ENV=production` + `AUTH_SIGNUP=invite` fecha o cadastro aberto da fase
    1: com host público, o OTP na tela era aceitável só em `ENV=local`.
  - O import só com backup e compatibilidade conferidos separa a validação
    da plataforma de um problema de dados, com caminho de volta.
- **Alternativas:**
  - O host proposto pelo planner (`planner-celld.djalmajr.dev`): rejeitado
    pelo operador, que escolheu `celld.djalmajr.dev`.
  - Importar o dump incluindo `d1_migrations`: sobrescreveria o estado de
    migração do destino (19) com o da origem (18).
  - Importar sem o backup completo: sem caminho de volta se o dump
    conflitar com o esquema.
- **Reverter:**
  - Variáveis: reestagiar o artefato com `ENV`/`APP_URL` da fase 1 e
    re-deployar; o Secret volta às chaves da C11.
  - Import: o backup `planner-20260926T031124Z.sql.gz` é do **`sqld` de
    origem** (permite reimportar), não um snapshot do D1 antes do import.
    O D1 estava vazio antes; voltar ao estado vazio exigiria procedimento
    próprio (recriação controlada), ainda não ensaiado.
  - Host: `kubectl delete` do IngressRoute `celld-planner` e do
    Certificate `celld-planner-tls` + remoção da A record (rollback
    documentado em `infra/celld/planner/README.md`). A fleet fica no ar e
    interna.
- **Onde:** `infra/celld/planner/` (worktree `infra-celld`, branch
  `feat/celld-pilot`) e o runbook 19 no ai-memory.
- **Status:** decisão registrada e fase 2 implantada em 2026-09-26, após
  autorização explícita do operador para Job e rota pública.
  - **Concluído** (executado pelo orquestrador em 2026-09-26):
    - A record `celld.djalmajr.dev` → `167.235.206.217` (`proxied=false`,
      TTL 60), criada via API Cloudflare e resolvendo por `@1.1.1.1`;
    - Certificate `celld-planner-tls` aplicado isoladamente: Ready, válido
      até 2026-12-25;
    - backup pré-import do `sqld` de produção
      (`planner-20260926T031124Z.sql.gz` no PVC de backups; o `sqld` não
      mudou);
    - compatibilidade conferida: origem com 18 migrações e 31 tabelas de
      aplicação, D1 destino com 19 migrações e 37 tabelas (as seis
      adicionais da 0019), colunas das 31 tabelas comuns coincidentes, e
      ensaio local do import em SQLite com integridade e FK válidas;
      preflight do D1 `planner-dev` com 19 migrações e 37 tabelas de app
      vazias;
    - único import do dump no D1 `planner-dev`: 12 INSERTs via
      `celld d1 execute --file /dev/stdin`, com `d1_migrations` preservada
      em 19. Pós-import: 9 tabelas com 12 registros de app (contagens
      iguais à origem) e `PRAGMA foreign_key_check` sem linhas. O endpoint
      D1 recusou `PRAGMA integrity_check` (`not authorized`): a checagem de
      integridade **no D1** não está concluída — o que passou foram o dump
      de origem e o ensaio local.
    - artefato `planner@7fb13d0` estagiado no nó com `ENV=production` e
      `APP_URL=https://celld.djalmajr.dev`; commit e variáveis públicas
      conferidos no destino, sem symlinks;
    - Secret da fase 2 com sete chaves: auth original preservado e seis
      novas chaves verificadas sem expor valores;
    - Job concluído após corrigir a permissão `0700` do diretório de
      estágio (o primeiro Job não conseguia ler `wrangler.json`). Versão
      celld `6d51c61343323ac9` adotada pelo nó, sem migrações novas;
    - recuperação de senha de conta importada avançou à tela de código
      sem OTP visível; o operador confirmou o e-mail recebido no horário
      do teste, sem registrar o código;
    - overlay público aplicado somente em `celld.djalmajr.dev`: HTTPS
      `/sign-in` 200 com certificado válido, host divergente 404, UI de
      cadastro por convite e pod pronto sem reinícios. Rotas HTTP de API
      key, OTP direto e root-token responderam 404;
    - teste de rede do pod Traefik: 8080 acessível, 8081 inacessível;
      a 8081 respondeu do host ao pod. O pod temporário usado para um
      diagnóstico de e-mail foi removido.
  - **Decidido:** host e URL HTTPS (`APP_URL`/`TRUSTED_ORIGINS`),
    `ENV=production`, `AUTH_SIGNUP=invite`, `AUTH_IP_HEADERS=x-real-ip`,
    e-mail pelo Cloudflare (três variáveis só do operador), preservação do
    `BETTER_AUTH_SECRET` e a regra do import (backup + compatibilidade
    antes; sem `d1_migrations`).
  - **Trabalho local** (commits `06ea11c` e `8c58992` no worktree
    `infra-celld`, sem push/PR; overlay público já aplicado): base
    `celld/planner/` privada; overlay irmão `celld/planner-phase2/` com
    IngressRoute `celld-planner` (host exato, `websecure`, serviço
    `celld-planner:8080`) e NetworkPolicy que admite o namespace `traefik`
    apenas na 8080 (8081 segue sem rota e inacessível do traefik);
    `stage-planner-artifact.sh` com `--var` restrito a
    `ENV`/`APP_URL`; `create-planner-vars-secret.sh --phase2` (mescla as
    seis chaves da fase 2 no Secret existente e preserva
    `CELLD_VAR_BETTER_AUTH_SECRET`); runbook passo a passo da fase 2 em
    `celld/planner/README.md`. A revisão independente final do recorte
    `celld/` passou sem achados; renders da base e do overlay e dry-runs
    estritos no servidor passaram. O commit `8c58992` registra o ajuste
    de permissão no script de estágio e a atualização do README feitos
    após o deploy; a simulação local do estágio com `--var` passou.
  - **Observação de e-mail:** chamadas diretas da API Cloudflare do Mac
    e do host VPS deram HTTP 401/código 10000; do pod `celld`, a mesma
    credencial com corpo propositalmente inválido passou pela autenticação
    e deu HTTP 400/código 10001. A causa da diferença de origem não foi
    determinada. O envio real pelo aplicativo foi confirmado pelo
    operador. Não houve ação no labdev nem deploy da IA no EdgeR.

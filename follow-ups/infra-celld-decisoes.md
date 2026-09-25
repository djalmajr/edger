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

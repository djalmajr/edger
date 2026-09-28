# Tenant routing e rollout ponderado — contrato local

**Estado em 2026-09-26:** implementação, smoke, integração com Tenancit real local e revisão independente na branch `feat/tenant-routing`; não há publicação desta capacidade. Ver [Epic 25](../epics/25-tenant-routing/00-overview.md) para backlog, riscos e critérios de aceite.

## Modelo de decisão

O EdgeR seleciona primeiro o app por host registrado ou caminho. Uma política opcional por **nome completo do app** governa duas decisões: quais slugs de tenant podem alcançar o app e como distribuir requests sem versão explícita entre versões elegíveis. `EDGER_TENANT_ROUTING_ENABLED` e `EDGER_WEIGHTED_ROUTING_ENABLED` são independentes e desligadas por padrão. Cada parte da política só tem efeito com sua flag ligada. Sem política, vale o roteamento atual. Uma URL com `@versão` contorna só o split, jamais a allowlist de tenant quando a flag de tenant estiver ligada.

O Tenancit é o diretório de hostname → tenantSlug. O EdgeR deve chamar somente `/v1/identify`, usando API client `tenant:identify` de serviço. `/v1/resolve` pode devolver segredos e não pertence ao caminho de roteamento. O slug confirmado é contexto de domínio, não prova de login do visitante. Um cliente capaz de enviar o header `Host` de um domínio permitido pode alcançar o app mesmo sem pertencer àquele tenant; o teste local com `Host: acme.e25.invalid` e sem DNS público demonstra essa fronteira. A allowlist organiza disponibilidade por domínio, mas não é ACL de usuário. O app continua responsável pela autenticação/autorização dos seus dados; uma barreira por pessoa/tenant exigirá contrato de token/IdP adicional.

Com `EDGER_TENANT_ROUTING_ENABLED=false`, o EdgeR não exige `EDGER_TENANCIT_IDENTIFY_URL` nem `EDGER_TENANCIT_TOKEN_FILE`, não chama o Tenancit e não aplica allowlists armazenadas. Com a flag ligada, as duas configurações são obrigatórias. O token é lido do arquivo no startup; após rotacionar o Secret, reinicie o processo para carregar a credencial nova. A flag de split pode ser ligada sozinha. Desligar tenant enquanto há allowlist persistida suspende o bloqueio desse app e precisa ser tratado como mudança operacional de acesso.

## Política e API administrativa local

```json
{
  "name": "app",
  "tenantAccess": { "mode": "allowlist", "tenants": ["acme"] },
  "traffic": {
    "versions": [
      { "version": "1.0.0", "weight": 80 },
      { "version": "2.0.0", "weight": 20 }
    ]
  }
}
```

`public` preserva o acesso aberto. `allowlist` vazia é inválida. Pesos são inteiros positivos e somam 100. Somente versões públicas, habilitadas e não staged podem participar. Host route requer que cada versão ponderada declare o próprio alias. O contrato local testado é `GET|PUT|DELETE /api/admin/routing-policy?name=<nome-completo-codificado>`; a query preserva `@scope/name`. GET devolve `{ "policy": ... }` ou `null` e exige `workers:read` mais visibilidade do app. PUT recebe o documento inteiro, exige root e não altera o snapshot se a validação ou gravação falhar. DELETE exige root e remove allowlist e split, retornando ao roteamento anterior. As duas flags governam a aplicação da política salva no data plane; salvar a política não liga as flags. Esta API ainda não foi publicada.

Requisições públicas `@versão` também passam pela allowlist quando a flag de tenant está ligada. Uma chamada interna com marcador e credencial root autenticada, como cron, não carrega domínio de visitante e pode invocar o worker sem identify; o mesmo marcador enviado sem credencial root não abre exceção.

## Sessão e rollback

Com a flag weighted ligada, o coorte é derivado de cookie opaco de sessão e nome do app. Com a flag desligada, não se emite esse cookie e vale o `defaultVersion` atual. O cookie não contém identidade de usuário nem versão; com a mesma política, a mesma sessão tem a mesma escolha. Mudar pesos pode remapear sessões, e remover a política retorna imediatamente ao `defaultVersion`. Um cliente sem cookies pode variar entre versões. O 80/20 representa distribuição esperada de **sessões**, não garantia de que 80% de todos os requests chegarão à versão A: sessões com tráfego desigual distorcem essa proporção.

## Falha e operação

Para app restrito, identidade ausente, divergente ou indisponível impede dispatch. A documentação Tenancit pede revalidação de `ETag` em cada uso para evitar servir mapeamento antigo após reatribuição de hostname. `304` só reutiliza identidade já guardada para o mesmo hostname canônico. `404` nega por domínio/tenant ausente; `401/403` do Tenancit são falha da credencial de serviço, e `429`/timeout/`5xx` são falhas de dependência, sem fallback para identidade velha. O feed atual de eventos traz referência de domínio sem hostname, e `/v1/tenants` não lista domínios; portanto ainda não oferece espelho offline seguro. Como o EdgeR revalida a cada request, dimensione o RPM do API client pela taxa de requests aos apps restritos, não pelo número de hostnames: no ensaio local, um cliente de 2 rpm permitiu duas chamadas e a terceira recebeu 503 no EdgeR após 429 do Tenancit. A escala, o rate limit e a recuperação no ambiente alvo ainda precisam ser medidos antes de habilitar apps restritos de alto volume. A credencial de serviço não pode viajar para o worker, aparecer em logs nem ser inserida no artefato ZIP.

No build local, `/metrics` inclui `edger_tenant_routing_{allowed,denied,unavailable}_total` sem labels de hostname, slug ou cookie. A indisponibilidade do identify responde 503; hostname/tenant ausente ou não permitido responde 404. Contagens por worker e versão continuam nas métricas do pool. Monitorar aumento de `unavailable` e comparar requests por versão antes de ampliar um rollout. O endpoint de métricas exige `observability:read` ou root.

Política em arquivo local é adequada à primeira instância, mas não sincroniza múltiplas réplicas. Antes de publicar a capacidade em uma implantação multi-réplica, definir um store/controle compartilhado e provar rollback consistente. Nenhum deploy está incluído nesta fase.

O diretório `.edger-routing` é rejeitado se for symlink no boot e nas mutações; o teste local cobre troca por symlink após o boot e compensação de DELETE em múltiplas raízes. O PVC operacional continua sendo área confiável do operador. Uma troca maliciosa do diretório exatamente entre a checagem e a operação de filesystem ainda exigiria operações por descriptor com `O_NOFOLLOW` para ser eliminada; esse cenário não foi validado como fronteira contra outro processo com escrita no mesmo PVC.

## Configurar e reverter

1. Instale as versões públicas do mesmo app e confira que estão habilitadas e não staged. Para tráfego por host, cada versão do split precisa declarar o alias; para plugin base, precisa declarar a mesma base. Mantenha as duas flags desligadas enquanto prepara a política.
2. Se for restringir por tenant, prepare no Tenancit um API client de serviço com somente `tenant:identify`. Entregue o token por um Kubernetes Secret existente, sem colocá-lo em values, no documento da política ou em variáveis de ambiente com o valor literal. No formulário Rancher, ligue **Enable Tenant Routing** e preencha **Tenancit Identify URL**, nome e campo do Secret. Esses três campos não são necessários com tenant routing desligado. **Enable Weighted Routing** é independente.
3. No detalhe do app no cPanel, confira o nome completo, versões elegíveis, slugs e soma dos pesos; confirme a gravação. A API equivalente é `PUT /api/admin/routing-policy?name=<nome-completo-codificado>` com o JSON acima e credencial root. Um GET no mesmo endereço confirma o documento armazenado. Gravar a política não liga flags no processo.
4. Habilite as flags desejadas na instalação e aguarde o rollout/restart do EdgeR. Com tenant ligado, teste host permitido, host não permitido e indisponibilidade do identify antes de ampliar tráfego. Com split ligado, teste duas sessões distintas e repetição da mesma sessão; acompanhe contagens por app/versão e `edger_tenant_routing_{allowed,denied,unavailable}_total`. O teste local desta entrega está em [evidência](../status/evidence/tenant-routing-2026-09-26.md).
5. Para **reverter apenas o split**, salve uma política com a **mesma** `tenantAccess` e `traffic.versions` contendo só a versão segura com peso 100. Remover o campo `traffic` também retorna ao `defaultVersion` e conserva a allowlist. Mudar pesos pode remapear sessões na próxima request.
6. `DELETE /api/admin/routing-policy?name=...` remove **split e allowlist juntos**. Use apenas quando também quiser tornar o app público conforme as flags ativas; não é rollback seguro de pesos para app restrito. Desligar a flag de tenant com uma allowlist gravada também suspende a barreira de disponibilidade.
7. Faça backup pela rota root `GET /api/admin/state/export` antes de alterações operacionais importantes. O ZIP inclui a política publicada em `.edger-routing` e exclui arquivos `.tmp`. A restauração é offline e segue o procedimento de [backup e restore do chart](../../../charts/edger/README.md#backup-and-restore); depois de subir o processo, confirme GET da política, flags, versão padrão e respostas HTTP. A rotação do Secret exige reinício para carregar o token novo.

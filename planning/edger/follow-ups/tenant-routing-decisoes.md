# Decisões e pendências — tenant routing (Epic 25)

Registro único de decisões da solicitação do operador em 2026-09-26. Cada decisão distingue o que foi pedido, o que é hipótese técnica e o que depende do Tenancit. Código e testes são a fonte de verdade para o estado implementado.

| ID | Estado | Decisão / alternativa considerada | Motivo e consequência |
|---|---|---|---|
| D25-01 | Confirmada pelo operador | Usar Tenancit como referência para identidade de tenant, em vez de inferir tenant do namespace do worker. | Tenancit já cadastra hosts e entrega `tenantSlug` em `/v1/identify`. |
| D25-02 | Confirmada pelo operador | Distribuição 80/20 deve ser estável por usuário/sessão. | Primeira implementação pode usar cookie de coorte sem PII; ausência de cookie impede stickiness. |
| D25-03 | Decisão de compatibilidade local | `@versão` continua fora do split, mas deve passar pelo gate de tenant. | Preserva smoke e rollback sem criar atalho para app restrito. |
| D25-04 | Decisão de segurança local | `Host` serve para selecionar domínio; `x-tenant-id` do cliente não autentica pessoa nem tenant. | EdgeR só confia na resposta autenticada de identify e substitui o header antes do worker. |
| D25-05 | Contrato confirmado com Tenancit | A primeira restrição é **disponibilidade do app por domínio/tenant**, não autorização de usuário. | Identify confirma o cadastro do hostname informado, não que a pessoa pertence ao tenant nem que o cliente não escolheu outro header `Host`. O smoke real local usou um Host permitido sem DNS público e alcançou o app. Se o produto pedir ACL de pessoas, é necessário contrato adicional de IdP/token. |
| D25-06 | Proposta para implementação | Política separada do manifesto de versão e do `defaultVersion`; update completo e atômico, com rollback por DELETE/100%. | Evita editar artefato imutável ou gravar ponteiro em cada request. |
| D25-07 | Contrato confirmado com Tenancit | Tenancit revalidado a cada uso conforme `ETag`/`Cache-Control: private, no-cache`; `304` só usa entrada anterior do mesmo hostname; erro nega apps restritos. | Evita identidade antiga após reatribuição, mas pode esgotar rate limit; requer medição antes de produção. |
| D25-08 | Proposta para implementação | Mutação de política root-only no primeiro corte; leitura filtrada por escopo. | `workers:promote` não deve poder retirar restrição por tenant. Uma permissão dedicada pode vir depois. |
| D25-09 | Contrato levantado; escala pendente | O feed de eventos atual não permite espelhar hostname → tenant: payload tem referência sem hostname, `/v1/tenants` não lista domínios e listagem de domínios é admin. | Resposta em `.herdr-agents/wF/from-tenancit-tenant-routing-design-20260926.md`. Para tirar identify do hot path, será necessário novo snapshot Consumer API + sequência/cursor de alterações, tombstones e reconstrução após lacuna. |
| D25-10 | Pendente | Multi-réplica e fonte durável compartilhada de políticas. | Arquivos locais não dão consenso entre instâncias; nenhum deploy dessa capacidade foi autorizado. |
| D25-11 | Confirmada pelo operador | Tenant routing e split A/B têm flags independentes, ambas desligadas por padrão. | Política salva fica inerte para cada função cuja flag esteja off; Tenancit não precisa de configuração quando tenant está off. Desligar tenant suspende a allowlist, portanto exige atenção operacional. |
| D25-12 | Confirmada pelo operador | As flags e os campos condicionais do Tenancit aparecem no formulário Rancher. | `questions.yaml` controla os dois opt-ins; tenant on exige URL identify e referência ao Secret existente, sem token em values. |

Alternativa descartada neste corte: usar somente namespace de API key EdgeR como tenant público. A API key atual protege o control plane e o `Authorization` de visitantes pertence ao worker; exigir a key no data plane quebraria apps existentes. Alternativa futura: token autenticado de usuário com claim de tenant, separado da identidade de domínio.

**Pendência entre projetos:** medir o tráfego projetado do EdgeR diante do rate limit `tenant:identify` do API client antes de habilitar allowlists de alto volume. Webhooks e `/v1/events` atuais não substituem a revalidação síncrona.

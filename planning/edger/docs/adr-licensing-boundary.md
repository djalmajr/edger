# ADR — Fronteira de licenciamento e distribuição

- **Status:** Aceito
- **Data:** 2026-07-12

## Contexto

O EdgeR precisa sustentar desenvolvimento, operação gerenciada e suporte sem
retirar do runtime Community os mecanismos necessários para segurança e
diagnóstico. O Epic 17 removeu o registry de extensões e consolidou um runtime
minimalista com workers soberanos; usar plugins premium dentro do processo
contrariaria essa arquitetura.

## Decisão

1. Versões futuras do monorepo serão publicadas sob O'Saasy 1.0 e descritas como
   source available.
2. O runtime Community permanece completo para execução segura e operação
   local, incluindo cPanel, health, logs e observabilidade local.
3. Diferenciais comerciais serão externos ao hot path e consumirão contratos
   públicos e versionados.
4. SDKs de cliente extraídos no futuro podem usar MIT ou Apache-2.0 quando isso
   favorecer adoção e não expuser o runtime completo.
5. Não será criado um registry genérico de plugins para monetização.

## Consequências

- terceiros podem estudar, modificar, distribuir e operar o EdgeR, respeitando
  a restrição contra oferta concorrente hospedada ou gerenciada;
- recursos de segurança e diagnóstico não se tornam paywalls artificiais;
- produtos comerciais evoluem com isolamento de falha e fronteiras explícitas;
- cópias já recebidas sob MIT mantêm os direitos daquela distribuição;
- a primeira release O'Saasy precisa registrar claramente o ponto de transição.

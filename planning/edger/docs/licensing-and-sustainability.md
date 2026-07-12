# Licenciamento e sustentabilidade

## Classificação

O EdgeR é distribuído sob a [O'Saasy License](../../../LICENSE). A licença
preserva permissões amplas para estudar, usar, modificar e distribuir o código,
mas reserva ao licenciante original a oferta concorrente do próprio EdgeR como
SaaS, serviço gerenciado ou cloud quando sua funcionalidade for o valor
principal.

Por conter uma restrição de campo de uso, o EdgeR é **source available**, não
open source aprovado pela OSI. README, documentação, imagens e comunicação do
produto devem usar essa classificação de forma consistente.

## Fronteira Community e comercial

O runtime Community deve continuar suficiente para uma instalação segura e
operável. Não podem ser artificialmente condicionados a uma edição paga:

- isolamento de workers e limites básicos de recursos;
- autenticação, autorização e proteção do control plane;
- roteamento e ciclo de vida de versões;
- health passivo e probes necessários à operação;
- logs e eventos locais no cPanel;
- métricas e observabilidade locais;
- exportação opcional por OTLP e endpoints compatíveis com Prometheus;
- cPanel, deploy local e diagnóstico básico.

Capacidades comerciais futuras podem viver em distribuições ou repositórios
privados externos ao hot path:

- gestão coordenada de fleets e múltiplos clusters;
- promoção, aprovação, assinatura e rollback coordenados;
- governança organizacional e políticas avançadas;
- retenção longa, arquivo, compliance e exportações especializadas;
- operator avançado para Kubernetes/Rancher, backup e disaster recovery;
- operação gerenciada, SLA e suporte.

Essas capacidades devem integrar-se por APIs versionadas, OTLP, webhooks,
Helm/operators ou processos separados. Não será reintroduzido um runtime
genérico de plugins nem acesso de terceiros à memória ou aos segredos internos
do processo.

## Transição a partir de MIT

A política O'Saasy governa versões futuras publicadas após sua adoção. Artefatos
e cópias anteriormente disponibilizados sob MIT continuam sujeitos aos direitos
que receberam. A primeira release sob a nova política deve registrar essa
fronteira no changelog e nas notas de release.

Esta documentação descreve a política técnica e de produto do repositório; não
substitui aconselhamento jurídico.

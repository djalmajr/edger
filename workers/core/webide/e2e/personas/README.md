# Personas do EdgeR WebIDE

Personas específicas do produto usadas pelos fluxos em `../flows/`. Cada fluxo
declara uma persona primária; ela define a motivação, o conhecimento prévio e os
critérios usados pelo skill `ux-persona` durante o walkthrough.

| ID | Papel | Foco principal |
| --- | --- | --- |
| `webide-first-time-author` | Autor iniciante | Descoberta, vocabulário, orientação e primeiro sucesso |
| `webide-power-developer` | Desenvolvedor frequente | Velocidade, atalhos, densidade e controle |
| `edger-platform-operator` | Operador da plataforma | Deploy, preview, logs e estado operacional confiável |
| `release-reliability-engineer` | Engenheiro de confiabilidade | Falhas seguras, recuperação e preservação de dados |
| `adversarial-security-researcher` | Pesquisador de segurança | Abuso pela UI, isolamento e ausência de vazamentos |
| `ui-ux-responsive-auditor` | Auditor UI/UX | Hierarquia visual, consistência e responsividade |
| `assistive-technology-developer` | Desenvolvedor com tecnologia assistiva | Teclado, leitor de tela, foco e anúncios |

## Limites da persona adversarial

O pesquisador adversarial usa apenas controles e entradas visíveis da WebIDE,
começando pelo entry point do fluxo. Ele testa rejeições e limites seguros sem
brute force, exploração destrutiva, acesso direto a rotas internas ou tentativa
de afetar dados fora dos fixtures. Essa lente encontra falhas observáveis pela
interface; não substitui threat model, revisão de código ou pentest do runtime.

## Cobertura cruzada recomendada

O frontmatter aceita uma única persona primária. Quando quisermos uma segunda
rodada sobre a mesma jornada, devemos criar um fluxo de auditoria com objetivo
próprio, em vez de trocar silenciosamente a persona do fluxo original. Bons
candidatos para essa rodada são:

- segurança: importação, paths do Explorer, validação, deploy, preview e terminal;
- UI/UX: dashboard, modal de templates, workbench, tabs, footer e breakpoints;
- acessibilidade: dashboard, dialogs, árvore, tabs, ações por ícone e terminal;
- confiabilidade: importação, autosave, falha de validação/deploy e logs.

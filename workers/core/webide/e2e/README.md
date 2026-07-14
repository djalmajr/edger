# WebIDE UX flows

Catálogo de jornadas de uso do frontend EdgeR WebIDE. Cada fluxo começa em
`http://127.0.0.1:19080/webide`, usa somente ações observáveis de interface e
pode ser entregue individualmente ao skill `ux-persona`.

As lentes do produto vivem em `e2e/personas/`; elas substituem as personas
genéricas do skill para representar conhecimento, risco e objetivos próprios de
uma WebIDE EdgeR. Cada fluxo declara exatamente uma persona primária.

Os fluxos são definições de uso, não testes automatizados de CI. Relatórios de
execução devem ser adicionados em `e2e/usability/` sem alterar os arquivos em
`e2e/flows/`.

## Pré-check comum

- Iniciar o EdgeR com o comando de desenvolvimento documentado no `AGENTS.md`.
- Confirmar resposta HTTP 200 em `/webide`.
- Usar um perfil de Browser que permita IndexedDB, localStorage e sessionStorage.
- Para fluxos de deploy, manter a sessão administrativa válida e o worker de
  referência disponível.
- Executar os fluxos sequencialmente: o Browser e o armazenamento local são
  recursos compartilhados.

## Catálogo

| ID | Meta principal | Persona | Passos |
| --- | --- | --- | ---: |
| `empty-dashboard-and-first-project` | Partir do estado vazio e criar o primeiro projeto | webide-first-time-author | 8 |
| `dashboard-and-project-navigation` | Navegar e localizar projetos | webide-first-time-author | 7 |
| `project-template-catalog` | Explorar e criar todos os starters suportados | webide-first-time-author | 13 |
| `import-project-folder` | Importar pastas Static, Routes e Fetch e rejeitar pasta inválida | webide-first-time-author | 13 |
| `dashboard-project-lifecycle` | Duplicar, renomear e excluir projetos | webide-power-developer | 12 |
| `workbench-layout-and-navigation` | Compreender e alternar as regiões do workbench | webide-first-time-author | 9 |
| `explorer-file-folder-lifecycle` | Criar, organizar, renomear e excluir paths | webide-power-developer | 17 |
| `editor-tabs-and-order` | Abrir, fechar, reordenar e restaurar tabs | webide-power-developer | 12 |
| `editor-autosave-and-persistence` | Editar e restaurar rascunho sem deploy | release-reliability-engineer | 10 |
| `project-search` | Buscar texto, case e regex no projeto | webide-power-developer | 12 |
| `lint-and-problems` | Produzir diagnósticos e navegar para problemas | webide-power-developer | 12 |
| `validation-and-safe-failure` | Validar projeto sem realizar deploy | release-reliability-engineer | 10 |
| `deploy-and-preview` | Implantar e operar o último preview válido | edger-platform-operator | 12 |
| `failed-deploy-preserves-preview` | Preservar preview anterior após falha | release-reliability-engineer | 10 |
| `footer-panels-and-order` | Usar e reordenar painéis inferiores | edger-platform-operator | 10 |
| `log-preservation` | Controlar preservação de logs entre deploys | edger-platform-operator | 11 |
| `operational-terminal` | Usar somente comandos operacionais permitidos | adversarial-security-researcher | 14 |
| `layout-resizing-and-responsive` | Redimensionar e usar layout compacto | ui-ux-responsive-auditor | 12 |
| `keyboard-and-dialog-accessibility` | Operar navegação e diálogos por teclado | assistive-technology-developer | 13 |

## Distribuição das personas

| Persona | Fluxos primários | Intenção da rodada |
| --- | ---: | --- |
| `webide-first-time-author` | 5 | Onboarding, descoberta e modelo mental |
| `webide-power-developer` | 5 | Eficiência cotidiana no editor |
| `edger-platform-operator` | 3 | Operação, deploy e observabilidade |
| `release-reliability-engineer` | 3 | Persistência e recuperação segura |
| `adversarial-security-researcher` | 1 | Limites e vazamentos observáveis pela UI |
| `ui-ux-responsive-auditor` | 1 | Consistência visual e breakpoints |
| `assistive-technology-developer` | 1 | Teclado, leitor de tela e foco |

## Matriz de cobertura

| Superfície ou comportamento | Fluxos |
| --- | --- |
| Estado inicial sem projetos e primeiro projeto | `empty-dashboard-and-first-project` |
| Dashboard, Dashboard/Projects e busca de projetos | `dashboard-and-project-navigation` |
| Linha inteira clicável e ações independentes | `dashboard-and-project-navigation`, `dashboard-project-lifecycle` |
| Catálogo Frontend/Backend/Fullstack e estados Ready/Planned | `project-template-catalog` |
| Static SPA, React, Vue, Fetch Handler e Routes Table | `project-template-catalog` |
| Importação, manifesto e entrypoint | `import-project-folder` |
| Duplicate, Rename e Delete de projeto | `dashboard-project-lifecycle` |
| Explorer, Search, editor, preview, footer e retorno ao dashboard | `workbench-layout-and-navigation` |
| Arquivos, pastas, nesting, collapse, menu e dialogs | `explorer-file-folder-lifecycle` |
| Tabs, tooltip, empty state, close e drag-and-drop | `editor-tabs-and-order` |
| Highlight, Tab, Ctrl/Cmd+S, autosave e IndexedDB | `editor-autosave-and-persistence` |
| Busca simples, case-sensitive, regex, regex inválida e shortcut | `project-search` |
| Lint HTML, YAML, JSON e delimitadores JS/TS | `lint-and-problems` |
| Validate positivo/negativo e Logs | `validation-and-safe-failure` |
| Deploy explícito, sete etapas, histórico, iframe e ações do preview | `deploy-and-preview` |
| Falha de deploy e retenção do último preview | `failed-deploy-preserves-preview` |
| Problems, Logs, Terminal, Deployments, toggles e ordem | `footer-panels-and-order` |
| Preserve logs desmarcado/marcado e persistência da opção | `log-preservation` |
| help, files, status, preview, validate, deploy, clear e comando inválido | `operational-terminal` |
| Três splitters, persistência e breakpoints compactos | `layout-resizing-and-responsive` |
| Escape, overlay, Enter/Espaço, foco, tooltips e atalhos | `keyboard-and-dialog-accessibility` |

## Fixtures

- `fixtures/import-static-spa/`: projeto válido com arquivo aninhado.
- `fixtures/import-routes-table/`: projeto válido com kind routes.
- `fixtures/import-fetch-handler/`: projeto válido com handler fetch.
- `fixtures/import-missing-entrypoint/`: manifesto válido que referencia um
  entrypoint ausente e deve ser rejeitado.

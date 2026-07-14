---
id: dashboard-and-project-navigation
name: Navegar e localizar projetos no dashboard
reference: planning/edger/epics/22-core-workers-webide/08-reference-workbench-layout.md
persona: webide-first-time-author
entry: "http://127.0.0.1:19080/webide"
preconditions:
  - EdgeR em execução no entry point
  - Existem pelo menos dois projetos locais com nomes e runtimes diferentes
design_refs:
  dashboard: "planning/edger/epics/22-core-workers-webide/08-reference-workbench-layout.md"
---

## User goal

Encontrar um projeto local e abrir seu workbench sem precisar conhecer URLs.

## Steps (each step is a UI ACTION + the expected result)

1. No entry point (`dashboard`), **observe a marca, a busca central, a navegação lateral, as ações e a tabela Recent projects** → o dashboard apresenta hierarquia clara e nenhuma contagem redundante no menu ou no título da tabela.
2. (`dashboard`) **clique em Projects** → a página passa a mostrar All projects mantendo busca e navegação.
3. (`dashboard`) **clique em Dashboard** → Recent projects e as ações New project e Import voltam a aparecer.
4. (`dashboard`) **preencha Search projects com parte do nome de um projeto** → somente projetos correspondentes permanecem na tabela.
5. (`dashboard`) **limpe Search projects** → todos os projetos da seção atual reaparecem.
6. (`dashboard`) **clique na célula Runtime de uma linha** → toda a linha funciona como link e o workbench do projeto é aberto.
7. No workbench, **clique no logo EdgeR no canto superior esquerdo** → o dashboard reaparece sem perder o projeto local.

## Expected result

O usuário alterna Dashboard/Projects, filtra projetos, abre qualquer projeto por
toda a linha e retorna ao dashboard usando somente controles visíveis.

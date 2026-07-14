---
id: deploy-and-preview
name: Implantar e operar o preview versionado
reference: planning/edger/epics/22-core-workers-webide/06-webide-deploy-preview.md
persona: edger-platform-operator
entry: "http://127.0.0.1:19080/webide"
preconditions:
  - EdgeR em execução no entry point com Admin API funcional
  - A sessão do Browser possui a API key administrativa válida
  - Existe um projeto válido ainda não implantado e com versão 1.0.0
design_refs:
  deploy-preview: "planning/edger/epics/22-core-workers-webide/06-webide-deploy-preview.md"
---

## User goal

Implantar explicitamente um rascunho e operar somente a última versão que passou
por todo o pipeline.

## Steps (each step is a UI ACTION + the expected result)

1. No entry point, **abra o projeto ainda não implantado** → o Preview mostra o estado vazio e o botão Deploy project.
2. **clique em Deployments no footer** → o painel informa `No deployments yet.`.
3. **clique em Deploy project dentro do estado vazio do Preview** → inicia um deploy explícito e o botão de deploy fica indisponível enquanto a operação executa.
4. **acompanhe o painel Deployments** → Validation, Packaging, Upload, Release / migrations, Health check, Activation e Complete aparecem e avançam em ordem.
5. **aguarde a conclusão** → o histórico registra Succeeded e o Preview passa a renderizar `/<name>@1.0.0` no iframe isolado.
6. **clique em Refresh preview** → o iframe é recarregado sem iniciar novo deploy.
7. **clique em Open in new tab** → a mesma versão implantada abre em uma nova aba sem expor a sessão administrativa.
8. **feche a aba externa, volte ao workbench e altere version no manifesto para `1.0.1`** → o rascunho muda, mas o Preview continua em 1.0.0.
9. **pressione Ctrl/Cmd+S e clique no ícone Deploy project do header** → um segundo pipeline explícito começa para 1.0.1.
10. **aguarde a conclusão e abra Deployments** → o novo registro Succeeded aparece antes do anterior.
11. **observe e interaja com o Preview** → o iframe agora usa `/<name>@1.0.1` e continua funcional.
12. **recarregue a página** → projeto, histórico e último preview bem-sucedido são restaurados.

## Expected result

Deploy só ocorre por ação explícita, expõe sete etapas e histórico, e o preview
isolado muda apenas após sucesso da versão solicitada.

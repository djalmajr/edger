---
id: failed-deploy-preserves-preview
name: Preservar o preview anterior quando o deploy falha
reference: planning/edger/epics/22-core-workers-webide/06-webide-deploy-preview.md
persona: release-reliability-engineer
entry: "http://127.0.0.1:19080/webide"
preconditions:
  - EdgeR em execução no entry point com Admin API funcional
  - A sessão do Browser possui a API key administrativa válida
  - Existe um projeto com preview bem-sucedido e manifesto válido
---

## User goal

Ter certeza de que um rascunho inválido nunca substitui a última versão saudável
do preview.

## Steps (each step is a UI ACTION + the expected result)

1. No entry point, **abra o projeto já implantado e interaja com seu Preview** → a versão saudável atual responde antes da tentativa de falha.
2. **anote visualmente a versão do deployment mais recente em Deployments** → existe um baseline Succeeded.
3. **abra `manifest.yaml`, altere entrypoint para `missing.js` e pressione Ctrl/Cmd+S** → o rascunho inválido é salvo localmente sem mudar o iframe.
4. **clique em Deployments e depois no ícone Deploy project** → o pipeline inicia e falha na etapa Validation.
5. **observe o novo item no histórico** → aparece Failed com a mensagem de entrypoint ausente.
6. **clique em Logs** → existe evento DEPLOY de erro e não existe mensagem de ativação bem-sucedida para a tentativa.
7. **observe e interaja novamente com o Preview** → a versão saudável anterior continua carregada e Open in new tab continua apontando para ela.
8. **restaure o entrypoint original no manifesto e pressione Ctrl/Cmd+S** → o projeto volta a ser válido sem deploy automático.
9. **clique em Validate project** → Logs registra sucesso, enquanto o preview ainda é o baseline anterior.
10. **abra Deployments** → o histórico conserva tanto o sucesso anterior quanto a tentativa Failed para auditoria local.

## Expected result

A falha fica observável em etapas, Logs e histórico, mas não altera nem remove o
último preview implantado com sucesso.

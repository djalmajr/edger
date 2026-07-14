---
id: validation-and-safe-failure
name: Validar projeto sem realizar deploy
reference: planning/edger/epics/22-core-workers-webide/06-webide-deploy-preview.md
persona: release-reliability-engineer
entry: "http://127.0.0.1:19080/webide"
preconditions:
  - EdgeR em execução no entry point
  - Existe um projeto descartável com manifesto válido
---

## User goal

Verificar manifesto e arquivos antes de implantar, com falhas claras e sem
efeitos remotos.

## Steps (each step is a UI ACTION + the expected result)

1. No entry point, **abra o projeto descartável e clique no ícone Validate project** → o footer abre em Logs e registra `Project validation passed.`.
2. **clique em Deployments** → nenhum deployment foi criado pela validação.
3. **abra `manifest.yaml`, remova temporariamente o campo name e clique em Validate project** → Logs registra `Manifest name and version are required` como erro.
4. **defina name como `Invalid Name` e valide** → Logs registra que o nome deve ser URL-safe.
5. **restaure um name válido, altere entrypoint para `missing.js` e valide** → Logs registra que o entrypoint deve existir.
6. **restaure o manifesto completo e válido e valide novamente** → o log de sucesso volta a aparecer.
7. **clique em Deployments** → o histórico continua vazio durante toda a sequência de validação.
8. **observe o Preview** → ele não mudou por causa de nenhuma validação.
9. **recarregue a página e abra Logs** → os eventos locais de validação persistidos permanecem disponíveis.
10. **confirme que o botão Deploy continua habilitado e separado de Validate** → validação é um preflight, não um deploy implícito.

## Expected result

Validate cobre manifesto, nome e entrypoint, registra sucesso/falha em Logs e
nunca cria deployment nem troca o preview.

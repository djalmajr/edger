---
id: editor-tabs-and-order
name: Operar e ordenar tabs do editor
reference: planning/edger/epics/22-core-workers-webide/08-reference-workbench-layout.md
persona: webide-power-developer
entry: "http://127.0.0.1:19080/webide"
preconditions:
  - EdgeR em execução no entry point
  - Existe um projeto com manifest.yaml, index.html e app.js
---

## User goal

Manter vários arquivos abertos, identificar paths truncados e organizar a ordem
de trabalho.

## Steps (each step is a UI ACTION + the expected result)

1. No entry point, **abra o projeto com três arquivos** → o workbench abre na tab restaurada.
2. No Explorer, **clique em cada arquivo ainda fechado** → uma tab por arquivo aparece sem duplicatas.
3. **passe o ponteiro sobre cada tab** → o title mostra o path completo, mesmo quando o label está truncado.
4. **clique em uma tab inativa** → ela se torna ativa e o editor mostra o arquivo correspondente.
5. **foque outra tab e pressione Enter** → a tab selecionada muda pelo teclado.
6. **arraste `manifest.yaml` para antes da primeira tab** → o indicador de drop aparece e a ordem muda após soltar.
7. **arraste a tab atual para depois da última** → o indicador after aparece e a segunda reordenação é aplicada.
8. **clique no botão Close de uma tab inativa** → somente essa tab fecha e o arquivo continua no Explorer.
9. **clique com o botão do meio em outra tab** → a tab fecha sem mudar o conteúdo do arquivo.
10. **feche todas as tabs restantes** → o editor exibe `Open a file from the Explorer.`.
11. **clique em um arquivo no Explorer** → a tab é recriada e o editor volta a mostrar seu conteúdo.
12. **recarregue a página** → seleção e ordem atual das tabs abertas são restauradas.

## Expected result

Tabs abrem uma vez, expõem path completo, aceitam mouse/teclado, podem ser
fechadas ou reordenadas e restauram seu estado após reload.

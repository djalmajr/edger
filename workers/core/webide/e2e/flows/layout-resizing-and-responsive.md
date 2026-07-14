---
id: layout-resizing-and-responsive
name: Redimensionar o workbench e usar viewports compactos
reference: planning/edger/epics/22-core-workers-webide/08-reference-workbench-layout.md
persona: ui-ux-responsive-auditor
entry: "http://127.0.0.1:19080/webide"
preconditions:
  - EdgeR em execução no entry point
  - O Browser permite redimensionar viewport e arrastar splitters
  - Existe um projeto com preview e footer visíveis
---

## User goal

Adaptar o espaço de autoria à tarefa e continuar usando a WebIDE em janelas
compactas.

## Steps (each step is a UI ACTION + the expected result)

1. No entry point, com viewport desktop, **abra o projeto preparado** → Explorer, editor, Preview e footer aparecem sem overflow horizontal da página.
2. **arraste o splitter do Explorer para a esquerda além do limite** → a largura para em 180 px e a árvore continua utilizável.
3. **arraste o splitter do Explorer para a direita além do limite** → a largura para em 420 px.
4. **deixe o Explorer em uma largura intermediária e arraste o splitter do Preview** → o preview varia somente entre 24% e 65% da área principal.
5. **arraste o splitter horizontal do footer** → sua altura varia somente entre 16% e 48%.
6. **recarregue a página** → as três medidas escolhidas são restauradas do armazenamento local.
7. **reduza a janela para aproximadamente 900 px de largura** → sidebar vira rail compacta, ações textuais secundárias somem e editor/preview continuam lado a lado.
8. **clique nos ícones Dashboard e Projects da rail compacta** → tooltips/labels acessíveis permitem navegar mesmo sem texto visível.
9. **abra novamente o projeto e reduza a janela para aproximadamente 700 px** → Explorer é ocultado e Preview passa para baixo do editor.
10. **clique nos toggles de Preview e panel** → ambos continuam alternáveis no layout compacto.
11. **use Search pelo atalho Ctrl/Cmd+Shift+F** → a busca continua acessível mesmo com Explorer visualmente recolhido.
12. **restaure o viewport desktop** → regiões voltam ao layout desktop sem perda do projeto ou do rascunho.

## Expected result

Splitters respeitam limites e persistem; breakpoints compactos reorganizam as
regiões sem impedir navegação, busca ou toggles.

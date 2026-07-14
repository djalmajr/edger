---
id: keyboard-and-dialog-accessibility
name: Operar navegação e dialogs por teclado
reference: planning/edger/epics/22-core-workers-webide/08-reference-workbench-layout.md
persona: assistive-technology-developer
entry: "http://127.0.0.1:19080/webide"
preconditions:
  - EdgeR em execução no entry point
  - Existe um projeto com ao menos dois arquivos
---

## User goal

Executar as jornadas principais com teclado, nomes acessíveis e foco previsível.

## Steps (each step is a UI ACTION + the expected result)

1. No entry point, **navegue com Tab até New project e pressione Enter** → o dialog abre e o primeiro template Ready recebe foco.
2. **pressione Escape** → o dialog fecha e o foco retorna para New project.
3. **abra o dialog novamente, navegue pelas categorias com Tab e ative Backend com Enter** → o tabpanel muda e `aria-selected` identifica a categoria ativa.
4. **pressione Tab até Close e ative-o** → o dialog fecha com título e descrição acessíveis.
5. **navegue até a linha de um projeto e pressione Enter no link Open project** → o workbench abre sem clique de mouse.
6. **navegue pelas quatro ações do header** → cada botão anuncia Show/Hide preview, Show/Hide panel, Validate project ou Deploy project.
7. **foque uma tab do editor e pressione Espaço** → o arquivo correspondente é selecionado.
8. **passe o ponteiro ou mantenha foco sobre uma tab truncada** → o title revela o path completo.
9. **pressione Ctrl/Cmd+Shift+F** → SEARCH abre e o input recebe foco.
10. **preencha uma consulta, navegue até um match e pressione Enter** → o arquivo abre e o cursor é posicionado na linha.
11. **volte ao Explorer, ative New file pelo teclado, preencha um nome e pressione Enter** → o dialog cria o arquivo e fecha.
12. **abra outro dialog de arquivo e clique em Cancel; abra-o novamente e clique no overlay fora do dialog** → as duas formas fecham o dialog sem aplicar alteração.
13. **use Ctrl/Cmd+S no editor e depois navegue ao logo pelo teclado** → o rascunho salva localmente e o dashboard volta a abrir.

## Expected result

Controles principais possuem nomes, foco e ativação por teclado; dialogs podem ser
abertos, confirmados ou cancelados sem mouse e o foco retorna a um ponto útil.

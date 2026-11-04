# Proposta visual: secrets por aplicativo

**Status:** proposta para revisão; não implementa nem altera produção.
**Ferramenta:** Open Pencil aberto pelo operador, documento editável `.fig`.
**Arquivo:** [edger-secrets.fig](../../../edger-secrets.fig), na raiz do projeto.
**Referência:** EDG-38, API de app secrets; solicitação do operador em 2026-11-04.
**Identidade:** não há `DESIGN.md` na raiz. Esta proposta usa o tema existente de
`workers/ui/src/theme.css`; este índice não cria um contrato de design de produção.

## Brief

- Produto em pt-BR; navegação Aplicações → Atlas → Secrets. Atlas e seus dados são fictícios.
- Noto Sans, correspondente à família Noto Sans Variable do console; texto 14/16 px,
  auxiliares 12/13 px, subtítulos 18/20 px e título 28 px.
- Superfícies branca e neutra quente; texto `#0c0c09`, borda `#e8e8e3`, marca `#bb4d00`,
  sidebar `#fbfbf9`; conversões sRGB dos tokens OKLCH existentes. Texto secundário
  `#626258` proposto para legibilidade. Destrutivo `#e7000b` do tema existente.
- Espaçamento em múltiplos de 4/8 px; controles de 40/44 px; raio visual 8 px.
- Tema escuro de referência com fundo `#0c0c09`, card `#1d1d16`, texto `#fbfbf9`.
- Valor preenchido só aparece mascarado; nunca representar um valor retornado pela API.

## Regras e rastreabilidade

- **BR-S01:** secrets pertencem ao nome do app e são compartilhados pelas versões;
  a API mantém o escopo do principal e exige `secrets:read` / `secrets:write`.
- **BR-S02:** listagem exibe nomes e datas; valores nunca são recuperados ou revelados.
- **BR-S03:** nome `^[A-Z_][A-Z0-9_]{0,63}$`, com prefixo `EDGER_` reservado;
  valor não vazio, até 16 KiB em bytes UTF-8; até 64 nomes por app. Sobrescrever
  um nome existente continua permitido. Nome já cadastrado exige intenção de
  substituir no cliente; a API faz upsert e não oferece CAS por valor.
- **BR-S04:** o manifest declara os nomes; valores são resolvidos no próximo spawn.
- **BR-S05:** gravação e exclusão não reciclam processos. Aplicação imediata exige
  operação explícita de reciclagem por versão e a permissão `workers:toggle`.
- **BR-S06:** cofre indisponível, falta de permissão, erro de rede, carregamento e
  vazio são situações diferentes; falhas não podem aparecer como lista vazia.
- **BR-S07:** exclusão exige confirmação e informa o efeito nos próximos processos.
- **BR-S08:** sucesso de escrita ou reciclagem não prova que cada processo usa um
  secret específico; não inventar status aplicado, histórico de valores ou auditoria nova.

## Fluxos

1. **Consultar:** `secrets-loading` → `secrets-list`; alternativos `secrets-empty`,
   `secrets-error`, `secrets-unavailable`, `secrets-denied`, `secrets-read-only`.
2. **Cadastrar:** `secrets-list` / `secrets-empty` → `secrets-create` →
   `secrets-create-validation` (quando inválido) → `secrets-saved`.
3. **Substituir:** `secrets-list` → `secrets-replace` → `secrets-saved`.
4. **Excluir:** `secrets-list` → `secrets-delete` → `secrets-list` / `secrets-empty`.
5. **Aplicar aos processos:** `secrets-saved` → `secrets-recycle` → `secrets-recycled`.
   Somente com permissão, escolhendo explicitamente uma versão. Cancelar não recicla.

## Telas e estados

Cada ID tem um frame e exatamente uma nota pareada ao lado dele no documento.

| ID | Página | Finalidade | Regras |
|---|---|---|---|
| `secrets-list` | 01 | Nomes, datas e ações por app | S01, S02 |
| `secrets-create` | 01 | Nome e valor novo | S02, S03, S04 |
| `secrets-create-validation` | 01 | Erro de campo sem perder o nome | S03 |
| `secrets-saved` | 01 | Confirma gravação e explica próximos processos | S04, S05, S08 |
| `secrets-replace` | 01 | Nome imutável e valor substituto | S02, S03 |
| `secrets-delete` | 01 | Confirmação destrutiva e dependência do manifest | S07 |
| `secrets-recycle` | 01 | Versão e impacto explícitos | S05 |
| `secrets-recycled` | 01 | Solicitação de reciclagem aceita | S05, S08 |
| `secrets-empty` | 02 | Cofre disponível, nenhum secret cadastrado | S06 |
| `secrets-loading` | 02 | Busca de metadados em andamento | S06 |
| `secrets-error` | 02 | Falha de comunicação e ação de tentar novamente | S06 |
| `secrets-unavailable` | 02 | Cofre não disponível nesta instância | S06 |
| `secrets-read-only` | 02 | Lista sem ações de escrita | S01 |
| `secrets-denied` | 02 | Sem permissão para listar | S01, S06 |
| `secrets-write-error` | 02 | Falha de gravação, nome preservado e valor limpo | S02, S03, S06 |
| `secrets-limit` | 02 | Limite de 64 nomes; substituição continua disponível | S03, S06 |
| `secrets-list-dark` | 03 | Mesma lista no tema escuro | S01, S02 |
| `secrets-list-mobile` | 03 | Lista em cards para largura 390 px | S01, S02 |

## Notas de integração

As notas do canvas trazem gatilho, ações e destino de cada frame. A navegação fora
de Secrets é contexto e fica fora do escopo. Os frames são uma proposta estática;
o arquivo não declara ligações interativas de protótipo.

Fechar o diálogo equivale a cancelar. Na implementação, os diálogos devem ter
nome acessível, foco contido, Escape para cancelar e retorno de foco ao gatilho.
Há um componente nativo de aviso de privacidade no topo da página 01, reutilizado
na lista principal e no estado de somente leitura. As demais telas reutilizam o
mesmo shell visual, sem criar um catálogo paralelo ao pacote de componentes.

As operações de dados existentes são GET da coleção e PUT/DELETE do secret em
`/api/admin/workers/{name}/secrets[/{secret}]`. A aplicação aos processos usa a rota
de recycle por versão, não um endpoint inventado de aplicação de secret.

A chave mestra, suas referências Kubernetes e valores reais ficam fora da interface
de app e deste documento. Nenhum dado de produção foi lido para montar o desenho.

## Validação

- [x] 18 frames de tela/estado e 18 notas pareadas conferidos contra este índice.
- [x] Ações e destinos conferidos, inclusive cancelamento e permissão de recycle.
- [x] Revisão visual por exports do Open Pencil, tema claro/escuro e largura 390 px.
- [x] Checks de acessibilidade do Open Pencil sem erros ou avisos nas quatro páginas;
  texto mínimo 12 px e alvo de fechar diálogos 44 px. Sem sobreposição entre
  frames e notas no nível superior. São checks do desenho, não testes de UI executável.
- [x] Arquivo nativo salvo e reaberto; hash em disco preservado na reabertura.
- [x] Escopo para versionamento restrito ao `.fig` e a este índice; alterações
  preexistentes em `.gitignore` e `CLAUDE.md` preservadas.

## Questões abertas

- Nome final da seção e aceite visual pelo operador.
- A tela não poderá inferir o estado de adoção de cada secret pelos processos.
- Habilitação do cofre em cada instalação deve ser indicada pela resposta da API;
  não presumir disponibilidade com base nos defaults do Helm.

## Próximo passo

Revisar a proposta com o operador e refinar a implementação no Cinzel. A aprovação
do arquivo visual não publica código nem autoriza operações em produção.

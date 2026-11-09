# Proposta visual: Secrets nos detalhes de um worker

**Status:** proposta corrigida para revisão; não implementa nem altera produção.
**Ferramenta:** Open Pencil aberto pelo operador, documento editável `.fig`.
**Arquivo:** [edger-secrets.fig](../../../edger-secrets.fig), na raiz do projeto.
**Referência:** EDG-38, API de app secrets; solicitação do operador em 2026-11-04.
**Identidade:** `DESIGN.md` registra a orientação confirmada pelo operador;
`workers/ui/src/theme.css` continua sendo a fonte dos tokens de produção.
Esta revisão substitui o shell incorreto da primeira proposta, que introduzia
um menu Aplicações. A navegação existente chama-se **Workers**.

## Brief

- Referência visual: o bundle público atual do console rc.30, servido localmente
  com API sintética. Nenhum dado de usuário ou credencial de produção foi usado.
- Entrada: **Workers → detalhes de demo-worker → Secrets**. `demo-worker`, sua
  versão 1.2.0 e os metadados do desenho são fictícios.
- Preservar Management/Operations/System, Overview/Workers/API Keys/Users,
  Observability, Active sessions e Settings conforme o principal. As abas existentes
  Files/Observability/Logs permanecem; a única aba nova é Secrets.
- Sidebar 216 px e header 64 px medidos; conteúdo com 16 px de padding; tabs de
  32 px. O header mantém nome, versão, retorno à lista e controles de preferência.
- A cópia visível segue a referência atual em inglês; as notas são em pt-BR.
- Noto Sans, correspondente à família Noto Sans Variable do console; texto de
  14 px, auxiliares 12/13 px e título do worker de 18 px.
- Superfícies branca e neutra quente; texto `#0c0c09`, borda `#e8e8e3`, marca `#bb4d00`,
  sidebar `#fbfbf9`; conversões sRGB dos tokens OKLCH existentes. Texto secundário
  `#626258` proposto para legibilidade. Destrutivo `#e7000b` do tema existente.
- Preservar controles compactos de 32 px e raios existentes; diálogos de 512 px
  usam o mesmo padrão visual. Alvo transparente de fechar tem 44 px.
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
Página 01: Worker · Secrets. Página 02: estados e permissões. Página 03: temas e
tela pequena. O ponto de entrada e o shell foram corrigidos em todos os frames.

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
O aviso de escopo e o shell seguem o mesmo padrão visual, sem criar um catálogo
paralelo ao pacote de componentes.

As operações de dados existentes são GET da coleção e PUT/DELETE do secret em
`/api/admin/workers/{name}/secrets[/{secret}]`. A aplicação aos processos usa a rota
de recycle por versão, não um endpoint inventado de aplicação de secret.

A chave mestra, suas referências Kubernetes e valores reais ficam fora da interface
de app e deste documento. Nenhum dado de produção foi lido para montar o desenho.

A rota de UI proposta conserva o contexto do detalhe
`/cpanel/workers/demo-worker/1.2.0/secrets`; ela ainda não existe no console.
A chamada de dados usa somente o nome do worker, sem versão. Header, navegação e
posição das abas foram comparados com a tela real de Files. A referência móvel
foi inspecionada a 390 × 844, sem um menu alternativo inventado.

## Validação

- [x] 18 frames de tela/estado e 18 notas pareadas conferidos contra este índice.
- [x] Ações e destinos conferidos, inclusive cancelamento e permissão de recycle.
- [x] Referência real do frontend renderizada localmente com dados sintéticos;
  inspeção visual e medidas de sidebar/header/tabs.
- [x] Shell e entrada corrigidos nos 18 frames, incluindo claro/escuro e 390 px.
- [x] Revisão final por exports representativos, estrutura e checks do Open Pencil:
  18 telas e 18 notas; sem overflow de texto ou sobreposição maior entre raízes;
  fontes Noto Sans disponíveis, sem substituição.
- [x] Contraste e tamanho de texto sem apontamentos. O preset completo de
  acessibilidade mantém 18 apontamentos de alvo de toque no container das abas
  existentes (32 px; recomendação do checker: 44 px). A densidade da referência
  foi preservada; o desenho não comprova acessibilidade da implementação.
- [x] Arquivo corrigido salvo e reaberto; SHA-256 preservado:
  `04100e20667e0e49e7c7af4e19ab0fa9fd7e7ae6452c3bc5a3b66d5d9bfe63dd`.
- [x] Versionar apenas a correção do desenho, índice e regras de layout aprovadas;
  preservar alterações preexistentes de `.gitignore` e `CLAUDE.md`.

O estado de limite mostra 15 dos 64 nomes, página 1 de 5, no mesmo padrão de
paginação do console. O aviso de escopo tem um componente nativo reutilizado
nas listas normal e somente leitura. A revisão foi feita sobre dados fictícios.

## Questões abertas

- Aceite visual da proposta corrigida pelo operador; nome e posição de Secrets
  já foram definidos por ele dentro dos detalhes de um worker.
- A tela não poderá inferir o estado de adoção de cada secret pelos processos.
- Habilitação do cofre em cada instalação deve ser indicada pela resposta da API;
  não presumir disponibilidade com base nos defaults do Helm.

## Próximo passo

Revisar a proposta com o operador e refinar a implementação no Cinzel. A aprovação
do arquivo visual não publica código nem autoriza operações em produção.

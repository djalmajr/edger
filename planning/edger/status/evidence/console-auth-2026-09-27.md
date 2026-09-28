# Evidência da autenticação da console — 2026-09-27

**Estado:** implementação integrada e verificada localmente. Login por senha e token, gestão de usuários, chart/Rancher, favicon e tela do cPanel estão prontos para validação do operador em `http://127.0.0.1:19080/cpanel/`. Nenhum commit, push, release ou deploy remoto foi realizado.

## Configuração Helm já verificada

- `helm lint charts/edger --set rootKey.existingSecret=existing-root` → exit 0, `1 chart(s) linted, 0 chart(s) failed` (aviso informativo do Helm: ícone de Chart recomendado).
- `helm template edger charts/edger --set rootKey.existingSecret=existing-root > /tmp/edger-auth-chart-default.yaml` → exit 0. Busca por `EDGER_ROOT_PASSWORD_FILE|console-root-password` não retornou linhas (exit 1 de `rg`, ausência esperada).
- `helm template edger charts/edger --set rootKey.existingSecret=existing-root --set consoleAuth.rootPasswordSecret.name=console-root > /tmp/edger-auth-chart-password.yaml` → exit 0. A saída contém `EDGER_ROOT_PASSWORD_FILE=/var/run/secrets/edger-console-root/password`, mount `console-root-password` e `secretName: "console-root"`; sem senha em ConfigMap/values.
- Com `--set consoleAuth.rootPasswordSecret.key=''` no template configurado → exit 1 esperado, erro `consoleAuth.rootPasswordSecret.key é obrigatório quando consoleAuth.rootPasswordSecret.name está configurado`.
- `git diff --check` → exit 0 no checkpoint de 2026-09-27; `refinement-lint.py --scope planning/edger --round console-auth-draft` → `VERDICT: PASS — 0 RED findings`.

Revisão AGY/Gemini do favicon e chart: `.herdr-agents/wF/reports/review-agy-console-20260927T131506.md`, 0 achados P1/P2; inspeção final do favicon na aba foi marcada `[partial]` e segue para o Browser integrado.

## UI e Browser local

`cd workers/core/cpanel && bun test src/components/login.test.tsx` → exit 0: 9 testes passaram, 0 falharam, 43 assertions (2026-09-27). Cobrem login por senha e token, 401/429/503/rede, copy conforme `login-options`, traduções e toggles. A prova integrada de autenticação aguarda o binário atualizado.

- `cd workers/core/cpanel && bun run build` → exit 0, `tsc --noEmit` limpo e bundle Vite gerado; aviso de chunk >500 kB já registrado no relatório do implementador `.herdr-agents/wF/reports/build-cpanel-login-qwen-20260927T135737.md`.
- A primeira execução de `bun test` teve 85 pass / 9 fail porque `routing-policy.test.tsx` fechava o happy-dom compartilhado antes dos testes de login. Removida apenas essa limpeza prematura; segunda execução **94 pass / 0 fail / 337 assertions** em 10 arquivos (2026-09-27). O relatório da primeira fatia documenta a falha original; a execução verde foi feita pelo orquestrador após a correção.
- No checkpoint inicial, Chrome local em `http://127.0.0.1:19080/cpanel/` mostrou a tela de login antes do novo binário. A prova final com binário atualizado e texto de root seedado está registrada abaixo.

## Backend root/sessão — checkpoint

- Relatório Pi/Qwen: `.herdr-agents/wF/reports/build-console-auth-qwen-20260927T142157.md`. `cargo +1.98.0 test -p edger-orchestrator --lib console_auth` → 14 pass, 0 fail; `--test console_auth` → 16 pass, 0 fail; `--bin edger` → 7 pass, 0 fail (inclui senha como única credencial e falha de DB existente sem degradar para open mode). `cargo +1.98.0 check -p edger-orchestrator --all-targets` → exit 0. A suíte completa do crate passou antes dos últimos testes de boot; o gate global será executado depois da gestão de usuários.
- Revisão independente AGY/Gemini inicial: `.herdr-agents/wF/reports/review-agy-console-20260927T142527.md`, 1 P2: Argon2 síncrono ocupava thread Tokio no login/troca de senha. A correção e sua prova integrada estão registradas abaixo.
- Na prova por mutação do implementador, uma cópia descartável usou `CARGO_TARGET_DIR` compartilhado; o cache de testes foi contaminado por artefato da mutação. Source principal foi verificado íntegro por hash e testes focados passaram após `cargo clean -p edger-orchestrator`. A limpeza removeu 46,1 GiB do cache e pode aumentar o tempo da próxima compilação. Nenhuma nova limpeza/mutação foi autorizada nesta sessão.

## Gestão de usuários — UI local

- Relatório Pi/Qwen: `.herdr-agents/wF/reports/build-console-users-ui-qwen-20260927T143401.md`. Página `/users` root-only, formulários de criar/editar/ativar/desativar/reset/excluir, troca da própria senha em sessão e cliente tipado. `cd workers/core/cpanel && bun test` → 124 pass, 0 fail, 952 assertions, 11 arquivos; `bun run build` → exit 0 (`tsc --noEmit && vite build`).
- O logout limpa `sessionStorage` antes da rede e limita a tentativa de revogação a 2 s; após o relatório, o orquestrador ajustou `main.tsx` para atualizar a tela de login imediatamente. A integração final do backend foi provada por HTTP abaixo.
- Revisão AGY/Gemini da gestão UI: `.herdr-agents/wF/reports/review-agy-console-20260927T143719.md`, 0 achados P1/P2; `bun test` 124/0 e `bun run build` verde. `bash planning/edger/scripts/cpanel-ui-gate.sh` → exit 0, `cpanel-ui-gate ok` (Vite emitiu avisos de depreciação dos plugins existentes e de chunk >500 kB).

## Integração final — backend, review e gates

- Backend Pi/Qwen: `.herdr-agents/wF/reports/build-console-users-backend-qwen-20260927T145611.md`. O SQLite migra o schema anterior sem perder root ou sessões; operadores têm grants próprios, e root é semeado quando ausente mesmo que operadores já existam. Criação, atualização, reset, exclusão e revogação de sessões são transacionais. Login, troca de senha, criação e reset fazem Argon2 em `spawn_blocking` sob limite compartilhado de quatro cálculos. Testes focados: 28 unitários, 16 HTTP de regressão e 8 HTTP de usuários, todos passaram.
- Revisão AGY/Gemini: `.herdr-agents/wF/reports/review-agy-console-20260927T150926.md`, `verdict: pass`, 0 achados P0–P3. O P2 anterior de Argon2 na thread Tokio foi corrigido e coberto por testes de saturação e resposta de `/livez` durante hashes lentos.
- `cargo +1.98.0 test --workspace` → exit 0; saída em `/tmp/edger-validation-wF/cargo-test-workspace.log`.
- `cargo +1.98.0 clippy --workspace -- -D warnings` → exit 0; saída em `/tmp/edger-validation-wF/cargo-clippy-workspace.log`.
- `cargo +1.98.0 fmt -- --check` → exit 0 após formatação dos três arquivos Rust da fatia de usuários.
- `bash planning/edger/scripts/cpanel-ui-gate.sh` → exit 0 após o último ajuste visual (`bg-sidebar`, borda e sombra do card); saída em `/tmp/edger-validation-wF/cpanel-ui-gate-final.log`. Os testes da UI permanecem 124 pass / 0 fail, e o build conclui.
- `helm lint charts/edger --set rootKey.existingSecret=existing-root` → exit 0, zero charts falhos. O template configurado e os casos negativo/default estão descritos acima.
- `/agile-refinement` Mode 1 no épico 26 e `python3 planning/edger/scripts/refinement-lint.py --scope planning/edger --round console-auth-final` → exit 0, `VERDICT: PASS — 0 RED findings`; há 46 avisos anteriores em outros épicos e nenhuma pendência vermelha. Saída em `/tmp/edger-validation-wF/refinement-console-auth-final.txt`.
- `SCRATCH=/tmp/edger-validation-wF/planning-gates RUSTUP_TOOLCHAIN=1.98.0 bash planning/edger/scripts/run-gates.sh` → exit 0, `ALL PLANNING GATES PASS`: Mode 1, oracle, path preflight, deploy layout, extensão, estrutura das histórias, cPanel, WebIDE, testes Bun e `cargo check`. Resumo em `/tmp/edger-validation-wF/planning-gates/run-gates.log`; relatório Mode 1 anexado a `planning/edger/status/evidence/refinement-report.txt`.

## Binário local e Browser

- `cargo +1.98.0 build -p edger-orchestrator --bin edger` → exit 0. O launcher descartável em `/tmp/edger-validation-wF/launch.py` reiniciou EdgeR em `127.0.0.1:19080` e o mock Tenancit em `127.0.0.1:19082`; o arquivo de senha de teste local fica em `/tmp/edger-validation-wF/root-password` com modo 0600. Seu valor não entra neste relatório.
- Smoke HTTP no binário: `login-options` confirmou senha habilitada/root semeado; senha do root gerou sessão; root token continuou aceito; root criou operador; operador autenticou com escopo não-root e recebeu 403 em `/api/admin/users`; desativação revogou sua sessão imediatamente (401); exclusão removeu o usuário de teste; `/cpanel/` respondeu 200. O script de prova terminou `PASS` e limpou o usuário temporário.
- Chrome local: tela de login em pt-BR renderizada com título, descrição condicionada ao root semeado, idioma/tema, campos, botão primário e entrada por token; fundo/card ajustados à referência do appliance. O navegador foi usado para validar a tela visível. O login funcional e a gestão foram provados por HTTP e testes DOM/API; o fluxo Browser autenticado permanece para validação manual do operador.
- `GET /cpanel/assets/favicon-5gj98C-v.svg` → `200 image/svg+xml`. Favicon também inspecionado visualmente em tamanho pequeno na revisão anterior.

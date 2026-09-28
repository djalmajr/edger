# Status de erro do store de autenticação fora do Admin REST

Na rc.5, o Admin REST distingue chave `egk_` inválida (401) de falha do
SQLite (503). O logout em modo aberto devolve 204 mesmo quando recebe um
header com formato de sessão e não há store para revogar.

O gate de `/api/mcp` usa `admin_api::authenticate`, mas ainda converte
qualquer erro de autenticação em HTTP 401. Assim, um `STORE_ERROR` causado por
lock após o timeout de 5 s não concede acesso, porém responde com status
incorreto. `authenticate_invoke` e alguns caminhos de `ControlAuth` usam a
interface `Option` e também perdem a distinção entre erro de store e chave
inválida.

Follow-up: propagar erros de store até os consumidores que precisam responder
503, preservando 401 para credenciais inválidas. Cobrir `/api/mcp`, invoke e
demais caminhos com testes de lock e de credencial inválida, sem modificar a
ordem de root key, `egk_`, sessão e OIDC. O risco atual é uma negação segura
com status incorreto; a revisão Grok da rc.5 não o considerou bloqueio para
lab-dev.

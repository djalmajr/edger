# Follow-up: worker CommonJS (`require`) falha — `require is not defined`

**Descoberto:** durante validação viva do Epic 20.01 (sandbox rede/cache).

**Sintoma:** `workers/commonjs-hono` (`index.js` com `require("@hono/node-server")`,
`package.json` com `"type":"commonjs"`, sem `node_modules` local) retorna HTTP 500:
`ReferenceError: require is not defined at .../index.js:1:11`.

**NÃO é regressão do sandbox rede/cache:** reproduz igual em `denoCacheMode: shared`
(comportamento pré-mudança). Independente do cache mode. Pré-existente (provável
desde o Epic 15/19).

**Causa provável:** o processo Deno persistente não roda o entrypoint `.js` em modo
CommonJS — não detecta o `"type":"commonjs"` do package.json sibling e/ou não
materializa `node_modules` para resolver `require(npm)`. `express-demo` (ESM
`import`) funciona; só o caminho `require` (CJS) quebra.

**Onde investigar:** `edger-isolation/src/multiproc.rs` (flags do spawn deno — falta
`--node-modules-dir=auto`? detecção de CJS?) e `multiproc_harness.mjs` (como o
módulo é importado). Deno 2.9 detecta CJS via package.json type, mas `require(npm)`
precisa de node_modules materializado.

**Escopo:** fora do Epic 20 (não é segurança/limites). Criar story de compat
CommonJS ou documentar que workers devem usar ESM. Baixa prioridade
(express/hono ESM cobrem o caso).

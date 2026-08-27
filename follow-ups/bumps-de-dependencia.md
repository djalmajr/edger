# Follow-ups: bumps de dependência adiados (triagem de 2026-08-27)

Onze PRs do dependabot foram avaliados um a um. Cinco entraram (actions de CI e
`tower-http`), três foram descartados por não se aplicarem (`rust-toolchain`
para uma versão que não existe, `rand` que não compila enquanto o `rsa` não
subir, `lru` que seria downgrade) e os três abaixo foram fechados porque valem
a pena mas **precisam de trabalho** — reabri-los como estavam só recria o
problema.

## `sha2` 0.10 → 0.11 — toca o contrato de hash das api-keys

`sha2` produz `sha256("edger-auth-v1:" || raw)`, o hash de toda key já emitida.
O digest não muda (SHA-256 é SHA-256), mas o 0.11 muda a API do RustCrypto: o
call site em `api_keys.rs` usa `format!("{:x}", hasher.finalize())`, e o
`LowerHex` some — a conversão para hex precisa ser reescrita, e errar isso
invalida silenciosamente as keys da instância.

O que fazer: trocar por um encoder hex explícito (`base16ct`, ou um laço de
duas linhas — o repo não tem `hex` no lock hoje), rebasar, e só aceitar com o
teste `hash_contract_is_stable` verde. Ele trava o digest de `"abc"`, então é
ele quem prova que nada mudou. Atenção: no PR original esse teste **nunca
rodou** — o `api_keys.rs` entrou na main depois de o PR ser aberto.

## `jsonwebtoken` 9 → 10 — falha só em runtime se passar batido

Quebra 14 testes de `oidc::tests::*` com panic de `CryptoProvider`: o 10.x
exige escolher o backend explicitamente. O detalhe perigoso é que o job
**Container image passa** — a imagem builda e embarca um binário que panica no
primeiro token OIDC verificado.

O que fazer: declarar o backend em `crates/edger-orchestrator/Cargo.toml`
(`jsonwebtoken = { version = "10", features = ["rust_crypto"] }`, preservando o
`use_pem` que o `oidc.rs` usa) e rodar a suíte inteira, não só o build.

## Toolchain 1.88 → 1.98 — precisa ser um bump só, em cinco lugares

O dependabot abriu dois PRs parciais e desalinhados: um levava a imagem base
para 1.97 e o outro a action para "1.100", versão que não existe.

Os cinco pontos que declaram a toolchain e precisam subir juntos:

* `Dockerfile` (estágio builder)
* `Dockerfile.cross`
* `.github/workflows/ci.yml` — três usos de `dtolnay/rust-toolchain@1.88`
* `Cargo.toml` da raiz — `rust-version = "1.88"`, herdado por todas as crates

Subir só um deixa o compilador do build diferente do compilador do CI.

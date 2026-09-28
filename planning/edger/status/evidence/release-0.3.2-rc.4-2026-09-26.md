# EdgeR v0.3.2-rc.4 — publicação no GHCR (2026-09-26)

## Escopo autorizado

O operador confirmou a tag `v0.3.2-rc.4` e limitou o deploy desta etapa à
publicação da imagem e do chart no GHCR. A VPS e o labdev não foram atualizados.

## Identidade e gates

- Tag anotada `v0.3.2-rc.4` aponta para
  `34c0cce017c879148bb762e122df5be9a377cc7c` no remoto.
- `Cargo.toml`, `Cargo.lock`, `charts/edger/Chart.yaml` e os manifests dos
  core workers `cpanel` e `webide` declaram `0.3.2-rc.4`.
- [Workflow da tag](https://github.com/djalmajr/edger/actions/runs/36250731572):
  conclusão `success`. Passaram Rust workspace, OTLP, contratos cPanel e
  planejamento, Helm, imagem, varredura de segredos, advisories e publicação.

## Artefatos conferidos no registro

- `helm show chart oci://ghcr.io/djalmajr/charts/edger --version 0.3.2-rc.4`
  mostrou `version` e `appVersion` iguais a `0.3.2-rc.4`. Digest do chart OCI:
  `sha256:0746e996b93d1b65b58d591659d1821b3a6283eab1e16ae1f7f35358e2816aaf`.
- `helm show values` desse chart mostrou `image.repository` como
  `ghcr.io/djalmajr/edger` e `image.digest` como
  `sha256:2549973c343a7d6d7a52225c092b65ecaa8e2a8f359e4c0cb2cba4d23abceda7`.
- `docker buildx imagetools inspect ghcr.io/djalmajr/edger:0.3.2-rc.4`
  mostrou o mesmo digest da imagem.

Esta evidência comprova publicação e coerência dos artefatos. Não inclui
smoke de uma instalação em execução, pois nenhum ambiente foi atualizado.

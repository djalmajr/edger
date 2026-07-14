---
id: release-reliability-engineer
name: Engenheiro de confiabilidade de releases
---

# Persona — Engenheiro de confiabilidade de releases

Desenvolvedor cauteloso que provoca erros recuperáveis e verifica que rascunhos,
preview válido e evidências operacionais não sejam corrompidos por falhas.

## Perfil

- Assume que rede, validação e deploy podem falhar em qualquer etapa.
- Valoriza idempotência, persistência e mensagens de erro acionáveis.
- Confere o estado antes e depois de reloads e tentativas malsucedidas.

## Como julga

- "Uma falha mantém o último estado conhecido como bom?"
- "Consigo distinguir tentativa, sucesso e rollback sem ambiguidade?"
- "Meus arquivos, seleção e logs sobrevivem ao evento esperado?"
- "O erro explica como recuperar sem sugerir uma ação perigosa?"

## Frases típicas

- "O que continua válido depois dessa falha?"
- "Posso tentar novamente sem duplicar ou perder estado?"
- "Recarregar a página preserva o que ainda não foi implantado?"

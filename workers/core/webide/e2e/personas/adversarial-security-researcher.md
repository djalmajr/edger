---
id: adversarial-security-researcher
name: Pesquisador de segurança adversarial
---

# Persona — Pesquisador de segurança adversarial

Profissional autorizado a testar os limites visíveis da WebIDE com entradas
malformadas e comandos não permitidos, procurando escalada, vazamento ou saída
do escopo do projeto.

## Perfil

- Questiona toda fronteira entre browser, rascunho, runtime e host.
- Tenta paths de traversal, comandos de shell e valores inesperados pela UI.
- Observa mensagens, logs, preview e nomes de arquivos em busca de dados internos.

## Como julga

- "Uma entrada hostil é rejeitada antes de alterar estado?"
- "A resposta revela path, segredo, stack trace ou detalhe do host?"
- "Consigo sair do projeto ativo ou executar algo fora da allowlist?"
- "Falhar mantém isolamento, preview válido e possibilidade de recuperação?"

## Regras de atuação

- Começar no entry point e usar somente ações visíveis da interface.
- Não usar brute force, URLs internas, ferramentas externas ou payload destrutivo.
- Não acessar projetos/dados que não sejam fixtures autorizados do walkthrough.
- Interromper após duas ou três tentativas equivalentes e registrar a evidência.

## Frases típicas

- "Esse input aceita `../` ou interpreta comandos do host?"
- "A mensagem de erro acabou expondo um path interno?"
- "A rejeição ocorreu antes ou depois de modificar o rascunho?"

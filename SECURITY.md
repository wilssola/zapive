# Política de segurança

O Zapive guarda conversas, mídia e credenciais de sessão do WhatsApp no computador do usuário. Falhas de segurança merecem tratamento cuidadoso.

## Versões com correções

Apenas a **última release** publicada em [Releases](https://github.com/wilssola/zapive/releases) recebe correções de segurança. O app se atualiza sozinho; peça a quem reporta que confirme o problema na versão mais recente.

## Como reportar uma vulnerabilidade

**Não abra uma issue pública.** Use o relato privado do GitHub:

1. Abra <https://github.com/wilssola/zapive/security/advisories/new>.
2. Descreva o impacto, os passos para reproduzir, a versão e o sistema operacional.
3. Se possível, inclua uma sugestão de correção.

**Nunca envie** conversas reais, números de telefone, tokens, o conteúdo de `vault.db`/`wa.db` ou chaves. Use dados fictícios; o modo `zapive --demo` roda com dados de exemplo e não conecta ao WhatsApp.

## O que esperar

- Confirmação de recebimento em até 7 dias.
- Avaliação e plano de correção em até 30 dias, conforme a gravidade.
- Crédito no aviso de segurança, se você quiser.
- Divulgação coordenada: publicamos o aviso junto com a release que corrige o problema.

## Escopo

Dentro do escopo: cofre local e criptografia em disco, índice de busca, atualizador (verificação do SHA-256), servidor MCP local (token e escuta apenas em `127.0.0.1`), tratamento de mídia e links recebidos, instaladores e o pipeline de release.

Fora do escopo: falhas do próprio WhatsApp ou de seus servidores, do sistema operacional, e ataques que exigem acesso físico ao computador já desbloqueado.

## Dependências

O CI executa `cargo audit` a cada pull request e semanalmente, e o Dependabot propõe atualizações de dependências e de actions.

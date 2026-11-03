## Por quê

Qual problema isto resolve? Link para a issue, se houver (`Closes #123`).

## O que mudou

Resumo da mudança. Se houver incompatibilidade, explique a migração.

## Como foi verificado

Comandos executados e plataformas testadas. Para mudanças de interface, inclua capturas feitas com `zapive --demo` (nunca com dados reais).

- [ ] `cargo fmt --all -- --check` nos arquivos alterados
- [ ] `cargo clippy --locked --all-targets` sem novos avisos
- [ ] `cargo test --locked`
- [ ] Textos novos passam pelo i18n (`i18n/en_US` e `i18n/pt`, ids semânticos)
- [ ] Commits seguem o padrão do `AGENTS.md` (`tipo(escopo): descrição`; `!` e `BREAKING CHANGE:` se incompatível)
- [ ] Nenhum segredo, token, conversa, nome ou número real no diff, nos logs ou nas capturas

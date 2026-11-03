# Contribuindo com o Zapive

Obrigado por querer ajudar. Este guia resume como compilar, verificar e enviar mudanças. Ao participar, você concorda com o [Código de Conduta](CODE_OF_CONDUCT.md). Falhas de segurança seguem a [SECURITY.md](SECURITY.md).

> **Regra de ouro:** nunca use dados reais em issues, testes, logs ou capturas de tela, e nunca envie mensagens, reações, status ou chamadas reais para testar. Use o modo demonstração.

## Compilar

Pré-requisitos: [Rust](https://rustup.rs) (o `rust-toolchain.toml` seleciona o canal `stable` com `rustfmt` e `clippy`).

| Plataforma | Extra |
| --- | --- |
| Windows | MSVC Build Tools e `./scripts/fetch-ctranslate2.ps1` (baixa a DLL oficial do CTranslate2 para `vendor/`) |
| Linux | `libasound2-dev libgtk-3-dev libxdo-dev cmake build-essential` |
| macOS | `cmake` (o CTranslate2 é compilado do código-fonte) |

```sh
cargo build --locked          # desenvolvimento
cargo run --locked            # abre o app
cargo build --release --locked
```

## Verificações antes do PR

```sh
cargo fmt --all -- --check    # formatação (veja a nota abaixo)
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
python3 -m unittest discover -s .github/scripts -p 'test_*.py'   # gerador de notas
```

O repositório ainda carrega um passivo de formatação e de avisos do clippy anteriores a estes checks; por isso o job `fmt and clippy` do CI é informativo. Não reformate arquivos inteiros que você não alterou: isso polui o diff. Garanta que **o seu código** passa, e que o `cargo test` e a compilação nas plataformas do CI continuam verdes.

O CI (`.github/workflows/build.yml`) roda build e testes em Linux, macOS e Windows, `cargo audit` e o teste do gerador de notas.

## Modo demonstração

`zapive --demo` abre o app com dados fictícios em um diretório de dados temporário e **nunca conecta ao WhatsApp**. Use-o para depurar a interface e para capturas de tela em issues e PRs, sem expor conversas reais.

## Textos da interface e i18n

Todo texto visível passa pelo i18n, com ids semânticos, nos arquivos `i18n/en_US/LC_MESSAGES/zapive.po` e `i18n/pt/LC_MESSAGES/zapive.po`. Não escreva textos fixos em `.slint` ou `.rs`.

## Código

- Identificadores e comentários de código em inglês.
- Siga o estilo do código vizinho; evite comentários que apenas repetem o código.
- Verifique as dependências existentes antes de adicionar novas, e justifique cada nova dependência no PR.

## Commits

O padrão completo está no [AGENTS.md](AGENTS.md); em resumo:

- Conventional Commits: `tipo(escopo): descrição`, com o tipo em inglês minúsculo e a descrição em português, no infinitivo e sem ponto final.
- Tipos: `feat`, `fix`, `perf`, `refactor`, `security` aparecem no changelog; `chore`, `ci`, `build`, `test`, `docs`, `style` ficam fora das notas.
- Um commit por mudança lógica, compilável, com implementação, traduções e testes.
- Incompatibilidades: `!` no título e `BREAKING CHANGE: ...` no corpo, explicando a migração.
- Não altere versões nem crie `chore(release)`: o pipeline faz isso.

### Como os commits viram release notes

Cada push em `master` gera uma release automaticamente (`.github/workflows/release.yml`). Quanto melhor o commit, melhor a nota:

- O **escopo** vira um rótulo em negrito (`**mensagens**: ...`).
- Em `feat`, `fix`, `perf` e `security`, o **primeiro parágrafo do corpo** aparece como sub-tópico. Escreva-o para o usuário final.
- O texto de `BREAKING CHANGE:` aparece na seção **Alterações importantes**, com a migração.
- Links para o commit e para o PR, a lista de contribuidores e o link de comparação são adicionados automaticamente.
- A release traz instaladores por plataforma, `SHA256SUMS.txt` e instruções de verificação.

Para ver o resultado localmente:

```sh
python3 .github/scripts/generate_changelog.py --to-tag HEAD --output notes.md
```

## Pull requests

- Parta de `master`, mantenha o PR focado e preencha o modelo.
- Descreva o que testou e em quais sistemas. Mudanças visuais pedem capturas (feitas com `--demo`).
- Não inclua segredos, `.env`, certificados, `vault.db`, `wa.db` ou logs sem sanitizar.

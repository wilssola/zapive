# Instruções para agentes

## Padrão de commits

- Use Conventional Commits: `tipo(escopo): descrição`.
- Escreva o tipo em inglês, em letras minúsculas, e a descrição em português, preferencialmente no infinitivo e sem ponto final.
- Use um escopo curto que identifique a área afetada, como `changelog`, `updates`, `messages`, `settings` ou `media`.
- Faça commits atômicos: uma mudança lógica por commit, incluindo implementação, traduções e testes necessários para que ela funcione. Separe mudanças independentes e mantenha cada commit compilável.
- Escreva descrições claras sobre o resultado da mudança. O changelog automático aproveita o título sem o prefixo técnico; não use títulos genéricos como “ajustes”, “melhorias diversas” ou “corrigir bug”.
- Use o corpo opcional para explicar motivação, comportamento e limitações. O corpo não é exibido como tópico no changelog.

### Tipos e categorias do changelog

| Tipo | Uso | Categoria |
| --- | --- | --- |
| `feat` | Nova funcionalidade | Novidades |
| `fix` | Correção de comportamento | Correções |
| `perf` | Melhoria de desempenho | Melhorias |
| `refactor` | Reestruturação sem nova funcionalidade | Melhorias |
| `security` | Correção ou melhoria de segurança | Segurança |
| `chore` | Manutenção interna | Omitido, salvo breaking change |
| `ci`, `build`, `test`, `docs`, `style` | Pipeline, build, testes isolados, documentação ou formatação | Omitidos, salvo breaking change |

Não classifique uma funcionalidade como manutenção para escondê-la do changelog. Evite tipos fora desta tabela; tipos desconhecidos aparecem em “Outras alterações”.

### Alterações incompatíveis e versionamento

- Marque alterações incompatíveis com `!` no título: `feat(storage)!: alterar o formato do histórico local`.
- Explique a incompatibilidade e a migração no corpo com `BREAKING CHANGE: ...`.
- Use sempre `!` no título quando houver incompatibilidade: o versionamento atual examina os títulos, enquanto o gerador de notas também reconhece o marcador no corpo.
- O pipeline atual incrementa major para títulos incompatíveis, minor para `feat` e patch para os demais commits elegíveis.
- Apenas `chore` sem `!` é excluído do cálculo de versão. Embora `docs`, `ci`, `build`, `test` e `style` sejam omitidos das notas, atualmente ainda podem provocar uma release patch.
- Não altere versões manualmente nem crie commits `chore(release)` por iniciativa própria: o pipeline de release controla essa etapa.

### Exemplos

```text
feat(updates): exibir novidades antes de baixar a atualização
fix(messages): preservar o texto quando o envio falhar
perf(media): reduzir o uso de memória das miniaturas
security(storage): impedir exportação de conteúdo temporário expirado
chore(deps): atualizar dependências de desenvolvimento
docs(agents): definir o padrão de commits do projeto
```

### Segurança e publicação

- Crie commits somente quando o usuário solicitar. Faça push apenas mediante solicitação explícita.
- Revise o diff e os arquivos staged antes de commitar; não inclua mudanças alheias à tarefa nem segredos, tokens, credenciais ou arquivos `.env`.
- Execute as verificações pertinentes e informe eventuais falhas ou validações não realizadas.
- Não use force push, não ignore hooks e não altere commits de terceiros com amend.

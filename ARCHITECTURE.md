# Arquitetura inicial do Codex Tray

O Codex Tray é um monitor leve do consumo do Codex associado a uma conta ChatGPT. O projeto separa a lógica de consulta e interpretação dos limites das integrações visuais de cada sistema operacional.

## Estrutura

```text
codex-tray/
|-- Cargo.toml
|-- ARCHITECTURE.md
|-- crates/
|   `-- codex-usage-core/          # cliente App Server e monitor compartilhado
|       `-- src/lib.rs
`-- apps/
    |-- codex-tray-windows/
    |   `-- src/main.rs
    `-- codex-tray-kde/
        `-- src/main.rs
```

### `codex-usage-core`

Biblioteca independente de interface gráfica e de sistema operacional. É responsável por:

- iniciar e acompanhar o processo `codex app-server`;
- comunicar-se com ele por JSON-RPC;
- consultar `account/rateLimits/read`;
- converter as respostas em modelos de dominio estaveis;
- controlar atualizações periódicas e tentativas após falhas;
- calcular o tempo restante ate cada reset;
- carregar configuracoes comuns e aplicar regras de alerta.

A biblioteca não depende de APIs de tray, Windows, KDE ou toolkit gráfico. Isso permite testar a lógica sem iniciar uma interface desktop.

O contrato público é `UsageSnapshot`: uma coleção ordenada de janelas de limite, cada uma com percentual usado, duração e horário de reset. O core prefere o formato atual `rateLimitsByLimitId` e preserva compatibilidade com o campo legado `rateLimits`.

### `codex-tray-windows`

Executável com a interface nativa do Windows. Cuida apenas de:

- icone e menu da area de notificacao;
- tooltips e notificacoes do Windows;
- conversão das ações do usuário em comandos para o core.

A implementação usa `tray-icon`, `tao` e o menu nativo do Windows. O ícone é verde abaixo de 70%, amarelo entre 70% e 89%, vermelho a partir de 90% ou quando a consulta falha. O menu contém o estado atual, `Atualizar agora` e `Sair`.

### `codex-tray-kde`

Executável para KDE Plasma. Cuida apenas de:

- integracao `StatusNotifierItem` por D-Bus;
- menu e notificacoes nativas do Plasma;
- conversão das ações do usuário em comandos para o core.

A implementação usa `ksni`, o protocolo nativo `StatusNotifierItem` sobre D-Bus. Ela é compilada somente no Linux e oferece status, tooltip, `Atualizar agora` e `Sair` no menu do Plasma.

## Fluxo de dados

```text
codex app-server
       |
       | JSON-RPC
       v
codex-usage-core
       |
       | UsageSnapshot / eventos
       +---------------------+
       |                     |
       v                     v
tray nativa do Windows   tray nativa do KDE
```

O core publica snapshots imutáveis. Cada frontend escolhe como representá-los, portanto as interfaces não precisam ter o mesmo layout nem os mesmos recursos.

## Modelo inicial

## Comunicação com o Codex

O core inicia `codex app-server` como processo-filho e usa JSON-RPC pela entrada e saída padrão. A sequência é:

1. enviar `initialize` com a identificação `codex_tray`;
2. enviar a notificação `initialized`;
3. consultar `account/rateLimits/read` imediatamente e depois a cada 60 segundos;
4. quando houver falha, informar a interface sem vazar detalhes de autenticação e tentar novamente após 15 segundos.

O aplicativo não lê, copia nem imprime arquivos de credencial. O App Server reutiliza o login já configurado no Codex. A consulta exige autenticação baseada nos serviços Codex/ChatGPT; uma chave da API sozinha não fornece esses limites de assinatura.

## Execução

Pré-requisitos:

- Rust estável;
- Codex CLI instalado e autenticado, com `codex` no `PATH`;
- no KDE, uma sessão Plasma com D-Bus de usuário disponível.

Caso o binário esteja em outro local, defina `CODEX_TRAY_CODEX_BIN` com o caminho completo. O intervalo é configurável por `CODEX_TRAY_INTERVAL_SECS`, entre 5 e 3.600 segundos; o padrão é 60.

```powershell
cargo run -p codex-tray-windows
```

```bash
cargo run -p codex-tray-kde
```

## Próximos passos sugeridos

1. Adicionar atualização imediata quando chegar a notificação `account/rateLimits/updated`, mantendo o polling como fallback.
2. Adicionar notificações nativas configuráveis para 70%, 90% e 100%.
3. Adicionar instalador e opção de inicialização automática para cada plataforma.
4. Criar builds assinadas/empacotadas para Windows e distribuições Linux.

## Comandos iniciais

```bash
cargo check --workspace
cargo test --workspace
cargo run -p codex-tray-windows
cargo run -p codex-tray-kde
```

Validações usadas no desenvolvimento:

```bash
cargo fmt --all
cargo check --workspace
cargo test --workspace
cargo check -p codex-tray-kde --target x86_64-unknown-linux-gnu
```

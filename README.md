# Codex Tray

Aplicativo de bandeja para acompanhar o uso do Codex no Windows e no KDE Plasma. O menu mostra o tempo restante até o reset e o percentual usado em cada janela, por exemplo: `2h35m: 35%/1d8h24m: 24%`.

## Pré-requisitos

- Rust stable e Cargo instalados via rustup (o projeto usa edition 2024).
- Codex CLI instalado e autenticado. Veja [Configuração](#configuração) para a localização do comando `codex`.
- Para KDE: Linux com uma sessão KDE Plasma e D-Bus de usuário disponível.
- Para Windows: Visual Studio Build Tools com as ferramentas de C++ e o Windows SDK.

Execute os comandos abaixo na raiz do repositório, no sistema operacional correspondente.

## Build para KDE

No Linux:

```bash
cargo build --release -p codex-tray-kde
```

Executável gerado: `target/release/codex-tray-kde`.

Para executar:

```bash
./target/release/codex-tray-kde
```

## Build para Windows

No PowerShell do Windows:

```powershell
cargo build --release -p codex-tray-windows
```

Executável gerado: `target\release\codex-tray-windows.exe`.

Para executar:

```powershell
.\target\release\codex-tray-windows.exe
```

Se o executável estiver em uso, encerre o aplicativo pelo menu **Sair** antes de recompilar.

As builds de release usam LTO, remoção de símbolos, uma unidade de geração de código e abort em caso de panic. Para gerar a build com target explícito e prioridade para tamanho, use a [seção de build otimizada](#build-com-prioridade-para-tamanho--windows-11-e-manjaro-kde).

## Iniciar automaticamente ao entrar no sistema

Faça a build primeiro e mantenha o executável em um caminho fixo. Se usar a build com target explícito, utilize os caminhos indicados abaixo.

### Windows

1. Pressione `Win + R`, digite `shell:startup` e pressione Enter.
2. Na pasta aberta, crie um **atalho** para `codex-tray-windows.exe`.
3. Nas propriedades do atalho, confira se **Destino** aponta para o executável e **Iniciar em** aponta para a pasta dele.

Para criar o atalho pelo PowerShell, execute na raiz do repositório após gerar a build Windows x86_64:

```powershell
$trayExecutable = (Resolve-Path -LiteralPath '.\target\x86_64-pc-windows-msvc\release\codex-tray-windows.exe' -ErrorAction Stop).Path
$startupFolder = [Environment]::GetFolderPath('Startup')
$shortcutPath = Join-Path $startupFolder 'Codex Tray.lnk'
$wshShell = New-Object -ComObject WScript.Shell
$shortcut = $wshShell.CreateShortcut($shortcutPath)
$shortcut.TargetPath = $trayExecutable
$shortcut.WorkingDirectory = [System.IO.Path]::GetDirectoryName($trayExecutable)
$shortcut.Description = 'Codex Tray'
$shortcut.Save()
```

Para a build sem `--target`, troque o caminho da primeira linha por `.\target\release\codex-tray-windows.exe`.

O aplicativo será iniciado no próximo login do seu usuário. Para desativar, remova o atalho da pasta `shell:startup`.

### Linux — Manjaro KDE

Abra **Configurações do Sistema → Inicialização automática (Autostart)**, selecione **Adicionar → Adicionar aplicativo** e escolha o executável `codex-tray-kde`.

Também é possível configurar pelo terminal. Execute na raiz do repositório, depois de gerar a build x86_64:

```bash
mkdir -p ~/.config/autostart
tray_binary="$(pwd)/target/x86_64-unknown-linux-gnu/release/codex-tray-kde"
cat > ~/.config/autostart/codex-tray.desktop <<EOF
[Desktop Entry]
Type=Application
Name=Codex Tray
Exec="$tray_binary"
Terminal=false
EOF
```

Para a build sem `--target`, troque o caminho na variável `tray_binary` por `$(pwd)/target/release/codex-tray-kde`.

O aplicativo será iniciado no próximo login na sessão gráfica. Para desativar, remova a entrada nas configurações de Autostart ou execute:

```bash
rm ~/.config/autostart/codex-tray.desktop
```

Confira a [configuração do Codex CLI](#configuração) para que o aplicativo consiga encontrá-lo na sessão ao fazer login.

## Build com prioridade para tamanho — Windows 11 e Manjaro KDE

Os targets abaixo são para processadores x86 de **64 bits (`x86_64`)**. Execute a build Windows no Windows 11 e a build KDE no Manjaro.

Estes comandos usam Rust stable e priorizam o tamanho do executável: `opt-level=z`, LTO completo, uma unidade de geração de código, remoção de símbolos, ausência de informações de debug e abort em caso de panic. As variáveis sobrescrevem o perfil apenas no ambiente do comando, sem precisar editar o `Cargo.toml`.

### Manjaro KDE — Bash

Instale as ferramentas de compilação, caso ainda não estejam disponíveis:

```bash
sudo pacman -S --needed base-devel
```

Com Rust instalado via rustup:

```bash
rustup target add --toolchain stable x86_64-unknown-linux-gnu
CARGO_PROFILE_RELEASE_OPT_LEVEL=z \
CARGO_PROFILE_RELEASE_LTO=fat \
CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1 \
CARGO_PROFILE_RELEASE_STRIP=symbols \
CARGO_PROFILE_RELEASE_DEBUG=0 \
CARGO_PROFILE_RELEASE_PANIC=abort \
CARGO_PROFILE_RELEASE_INCREMENTAL=false \
cargo +stable build --locked --release -p codex-tray-kde --target x86_64-unknown-linux-gnu
```

Executável: `target/x86_64-unknown-linux-gnu/release/codex-tray-kde`.

Para consultar o tamanho em bytes:

```bash
stat -c '%s bytes' target/x86_64-unknown-linux-gnu/release/codex-tray-kde
```

### Windows 11 — PowerShell

Com Rust instalado via rustup e as ferramentas MSVC indicadas nos pré-requisitos:

```powershell
rustup target add --toolchain stable x86_64-pc-windows-msvc
& {
    $buildSettings = @{
        CARGO_PROFILE_RELEASE_OPT_LEVEL = 'z'
        CARGO_PROFILE_RELEASE_LTO = 'fat'
        CARGO_PROFILE_RELEASE_CODEGEN_UNITS = '1'
        CARGO_PROFILE_RELEASE_STRIP = 'symbols'
        CARGO_PROFILE_RELEASE_DEBUG = '0'
        CARGO_PROFILE_RELEASE_PANIC = 'abort'
        CARGO_PROFILE_RELEASE_INCREMENTAL = 'false'
    }
    $previousSettings = @{}
    try {
        foreach ($name in $buildSettings.Keys) {
            $previousSettings[$name] = [Environment]::GetEnvironmentVariable($name, 'Process')
            [Environment]::SetEnvironmentVariable($name, $buildSettings[$name], 'Process')
        }
        cargo +stable build --locked --release -p codex-tray-windows --target x86_64-pc-windows-msvc
    }
    finally {
        foreach ($name in $previousSettings.Keys) {
            [Environment]::SetEnvironmentVariable($name, $previousSettings[$name], 'Process')
        }
    }
}
```

Executável: `target\x86_64-pc-windows-msvc\release\codex-tray-windows.exe`.

Para consultar o tamanho em bytes:

```powershell
(Get-Item .\target\x86_64-pc-windows-msvc\release\codex-tray-windows.exe).Length
```

### Tamanho e RAM

`z` é um ponto de partida para reduzir tamanho. Para encontrar o menor executável neste projeto, compare também com `s`, trocando o valor de `CARGO_PROFILE_RELEASE_OPT_LEVEL` e anotando o tamanho após cada build. A documentação do Cargo ressalta que `z` nem sempre produz um binário menor que `s`.

Essas opções não garantem o menor consumo de RAM em execução. O aplicativo também inicia um processo `codex app-server`; para avaliar o consumo total, meça o tray e esse processo filho. LTO pode aumentar o consumo de RAM durante a compilação.

Referência: [perfis e otimizações do Cargo](https://doc.rust-lang.org/cargo/reference/profiles.html).

## Configuração

O aplicativo inicia `codex app-server` e utiliza o login existente do Codex CLI para consultar os limites de uso da conta.

O comando `codex` deve estar no `PATH` da sessão. No Linux, quando não está no `PATH`, o aplicativo também procura instalações em `NVM_DIR` e `~/.nvm`, dando preferência à versão indicada pelo alias default e usando a versão mais recente disponível como alternativa. A pasta `bin` da instalação encontrada é adicionada ao `PATH` do processo filho.

Defina `CODEX_TRAY_CODEX_BIN` com o caminho completo do executável para substituir a detecção automática. Para startup, essa variável precisa estar disponível na sessão de login.

O uso é atualizado a cada 60 segundos. A variável `CODEX_TRAY_INTERVAL_SECS` aceita valores entre 5 e 3.600 segundos; valores inválidos usam o intervalo padrão. Após uma falha na consulta, o aplicativo tenta novamente após 15 segundos.

## Desenvolvimento

Para compilar e executar em modo de desenvolvimento, use o comando correspondente ao seu sistema:

```bash
cargo run -p codex-tray-kde
```

```powershell
cargo run -p codex-tray-windows
```

Comandos auxiliares (a compilação do frontend Windows deve ser feita no Windows):

```text
cargo fmt --all
cargo check --workspace
cargo test --workspace
```

## Estrutura do projeto

- `apps/codex-tray-kde`: interface de bandeja para KDE Plasma no Linux.
- `apps/codex-tray-windows`: interface de bandeja nativa do Windows.
- `crates/codex-usage-core`: cliente do Codex App Server e monitor de uso compartilhados.

Veja [ARCHITECTURE.md](ARCHITECTURE.md) para detalhes da implementação.

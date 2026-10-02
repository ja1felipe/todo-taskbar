# Todo Taskbar

Lista de tarefas na bandeja do sistema. Clique no ícone para abrir, adicione
tarefas, organize em abas e conclua com um clique — o horário de conclusão fica
registrado no fuso de Brasília. Tudo é salvo localmente em SQLite.

## Download

Pegue o instalador da sua plataforma na página de
[Releases](https://github.com/ja1felipe/todo-taskbar/releases).

### Linux

| Formato | Como instalar |
| --- | --- |
| `.deb` | `sudo apt install ./*.deb` |
| `.AppImage` | `chmod +x ./*.AppImage && ./*.AppImage` |

Dependências em tempo de execução: `libwebkit2gtk-4.1-0` e `libgtk-3-0`. No
Ubuntu 22.04 ou inferior, o pacote disponível é o `libwebkit2gtk-4.0-dev`.

O ícone na bandeja usa XEmbed, que é um protocolo de X11. Em sessões Wayland
puras o ícone pode não aparecer; nesse caso rode com
`GDK_BACKEND=x11 todobar` (ou `GDK_BACKEND=x11 ./*.AppImage`).

### Windows

Baixe o instalador `*setup.exe` (NSIS) ou o `.msi` e execute. O app fica na
bandeja; clique esquerdo abre a janela, clique direito mostra o menu com
**Abrir** e **Sair**.

### macOS

Baixe o `.dmg`, abra e arraste o `Todo Taskbar.app` para a pasta Aplicativos.

Os artefatos não são assinados nem notarizados, então o Gatekeeper vai
bloquear na primeira abertura. Para liberar: clique com o botão direito no app
→ **Abrir** → **Abrir**, ou em **Ajustes do Sistema → Privacidade e Segurança**
confirme **Abrir mesmo assim**. O arquivo `*.app.tar.gz` é a versão portátil.

## Uso

- **Clique esquerdo** no ícone: mostra/esconde a janela
- **Clique direito**: menu com **Abrir**, **Abrir ao inicializar** e **Sair**
- **Abrir ao inicializar**: faz o app iniciar junto com o sistema (a janela
  continua começando oculta, só o ícone aparece na bandeja). No Linux, prefira
  instalar via `.deb`/`.rpm`; rodando pelo AppImage o caminho apontado é
  temporário e some na próxima execução.
- **`Esc`** ou clicar fora: esconde a janela
- **`Ctrl+Q`**: encerra o app
- Arraste a borda esquerda da janela para redimensionar (a largura é lembrada)

O banco fica no diretório de dados do sistema, em `com.felipe.todobar/todobar.db`:

- Linux: `~/.local/share/com.felipe.todobar/todobar.db`
- Windows: `%APPDATA%\com.felipe.todobar\todobar.db`
- macOS: `~/Library/Application Support/com.felipe.todobar/todobar.db`

## Atualizações

O app consulta a última release ao iniciar e a cada 6 horas. Quando há versão
nova, aparece uma faixa no topo com o botão **Atualizar**, que baixa e instala
a atualização. Também dá para forçar a checagem pelo link **verificar** no
rodapé.

- **Windows** e **macOS**: atualizam direto, sem intervenção.
- **Linux (AppImage)**: atualiza direto, sem senha.
- **Linux (.deb/.rpm)**: o updater chama `dpkg`/`rpm` e pede a senha de
  administrador (via `pkexec`/`zenity`). Em builds de desenvolvimento, que não
  pertencem a nenhum pacote, o botão vira **Baixar** e abre a página da release.

## Desenvolvimento

Requisitos: Node 22+, Rust estável e as
[dependências do Tauri](https://tauri.app/start/prerequisites/).

```bash
npm install
npm run tauri dev
```

Para gerar o instalador local. Como `createUpdaterArtifacts` está ligado, o
build exige a chave privada do updater (mesmo para uso local):

```bash
export TAURI_SIGNING_PRIVATE_KEY=~/.tauri/todobar.key
npm run tauri build
```

Os artefatos saem em `src-tauri/target/release/bundle/`.

### Testes

```bash
cargo test --manifest-path src-tauri/Cargo.toml
npm run check
```

## Release

O workflow em `.github/workflows/release.yml` compila para Linux, Windows e
macOS (Intel e Apple Silicon) e publica os artefatos na release do GitHub.

Para publicar uma versão:

1. Atualize a versão em `package.json`, `src-tauri/Cargo.toml` e
   `src-tauri/tauri.conf.json` (os três precisam estar iguais).
2. Crie e envie a tag:

   ```bash
   git tag v0.2.0
   git push origin v0.2.0
   ```

3. O workflow roda sozinho e cria a release. Para disparar manualmente, use
   **Actions → Release → Run workflow** informando a tag.

### Segredos do repositório

O updater assina as atualizações com um par de chaves (geradas uma vez com
`npx tauri signer generate -w ~/.tauri/todobar.key`). Configure em
**Settings → Secrets and variables → Actions**:

- `TAURI_SIGNING_PRIVATE_KEY` — conteúdo de `~/.tauri/todobar.key`
- `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` — senha da chave (em branco se não houver)

Guarde a chave privada em local seguro: perdê-la impede que as instalações já
existentes recebam atualizações futuras.

Para publicar como rascunho (revisar os artefatos antes de tornar público),
troque `releaseDraft: false` por `true` em `.github/workflows/release.yml`.

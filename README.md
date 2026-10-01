# todobar

Lista de tarefas na bandeja do sistema. Clique no ícone para abrir, adicione
tarefas, organize em abas e conclua com um clique — o horário de conclusão fica
registrado no fuso de Brasília. Tudo é salvo localmente em SQLite.

## Download

Pegue o instalador da sua plataforma na página de
[Releases](https://github.com/ja1felipe/todo-taskbar/releases).

### Linux

| Formato | Como instalar |
| --- | --- |
| `.deb` | `sudo apt install ./todobar_*_amd64.deb` |
| `.AppImage` | `chmod +x todobar_*.AppImage && ./todobar_*.AppImage` |

Dependências em tempo de execução: `libwebkit2gtk-4.1-0` e `libgtk-3-0`. No
Ubuntu 22.04 ou inferior, o pacote disponível é o `libwebkit2gtk-4.0-dev`.

O ícone na bandeja usa XEmbed, que é um protocolo de X11. Em sessões Wayland
puras o ícone pode não aparecer; nesse caso rode com
`GDK_BACKEND=x11 ./todobar`.

### Windows

Baixe `todobar_*_x64-setup.exe` (NSIS) ou o `.msi` e execute. O app fica na
bandeja; clique esquerdo abre a janela, clique direito mostra o menu com
**Abrir** e **Sair**.

### macOS

Baixe o `.dmg`, abra e arraste o `todobar.app` para a pasta Aplicativos.

Os artefatos não são assinados nem notarizados, então o Gatekeeper vai
bloquear na primeira abertura. Para liberar: clique com o botão direito no app
→ **Abrir** → **Abrir**, ou em **Ajustes do Sistema → Privacidade e Segurança**
confirme **Abrir mesmo assim**. O arquivo `*.app.tar.gz` é a versão portátil.

## Uso

- **Clique esquerdo** no ícone: mostra/esconde a janela
- **Clique direito**: menu com **Abrir** e **Sair**
- **`Esc`** ou clicar fora: esconde a janela
- **`Ctrl+Q`**: encerra o app
- Arraste a borda esquerda da janela para redimensionar (a largura é lembrada)

O banco fica no diretório de dados do sistema, em `com.felipe.todobar/todobar.db`:

- Linux: `~/.local/share/com.felipe.todobar/todobar.db`
- Windows: `%APPDATA%\com.felipe.todobar\todobar.db`
- macOS: `~/Library/Application Support/com.felipe.todobar/todobar.db`

## Desenvolvimento

Requisitos: Node 20+, Rust estável e as
[dependências do Tauri](https://tauri.app/start/prerequisites/).

```bash
npm install
npm run tauri dev
```

Para gerar o instalador local:

```bash
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
   git tag v0.1.0
   git push origin v0.1.0
   ```

3. O workflow roda sozinho e cria a release. Para disparar manualmente, use
   **Actions → Release → Run workflow** informando a tag.

Para publicar como rascunho (revisar os artefatos antes de tornar público),
troque `releaseDraft: false` por `true` em `.github/workflows/release.yml`.

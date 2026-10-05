// Encaminha para a CLI do Tauri instalada na raiz do projeto.
//
// As tarefas Gradle do projeto Android (`src-tauri/gen/android/buildSrc`)
// executam `node tauri android android-studio-script` com o diretório de
// trabalho em `src-tauri`. O Node resolve o entrypoint apenas como caminho
// relativo ao cwd (`.js`, `.json` ou `.node`), sem procurar em `node_modules`,
// então este arquivo existe só para tornar a CLI alcançável a partir dali.
import { createRequire } from 'node:module';

createRequire(import.meta.url)('../node_modules/@tauri-apps/cli/tauri.js');

<script lang="ts">
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { invoke } from "@tauri-apps/api/core";
  import { onMount, onDestroy } from "svelte";
  import { check, type Update } from "@tauri-apps/plugin-updater";
  import { relaunch } from "@tauri-apps/plugin-process";
  import { openUrl } from "@tauri-apps/plugin-opener";
  import { createClient, type SupabaseClient, type RealtimeChannel } from "@supabase/supabase-js";
  import Section from "$lib/Section.svelte";
  import TodoItem from "$lib/TodoItem.svelte";

  type Todo = {
    id: number;
    text: string;
    done: boolean;
    /** Horário de conclusão em Brasília (ISO 8601 com offset), definido pelo
     *  backend. O cliente nunca escreve esse campo. */
    completedAt: string | null;
  };

  type Tab = {
    id: number;
    name: string;
    todos: Todo[];
  };

  type Store = {
    tabs: Tab[];
    activeId: number;
  };

  /** Conta conectada; `email` pode faltar se o provedor não fornecer. */
  type AuthInfo = {
    userId: string;
    email: string | null;
  };

  /** Dados do canal de Realtime entregues pelo backend. */
  type RealtimeConfig = {
    url: string;
    anon: string;
    accessToken: string;
    userId: string;
  };

  const MAX_TABS = 5;
  const MAX_NAME = 10;
  const MIN_WIDTH = 300;
  const WIDTH_KEY = "todobar.windowWidth";
  // Rede de segurança para o `localStorage` poder vir corrompido ou editado à
  // mão; o Rust ainda impõe o teto real de 1/4 da tela.
  const MAX_STORED_WIDTH = 2000;
  const DEFAULT_TAB: Tab = { id: 1, name: "Tarefas", todos: [] };

  const appWindow = getCurrentWindow();

  // Começamos com a aba padrão e substituímos quando o banco responder, para
  // não piscar um estado vazio na tela.
  let tabs = $state<Tab[]>([{ ...DEFAULT_TAB }]);
  let activeId = $state(DEFAULT_TAB.id);
  let ready = $state(false);
  let newTodo = $state("");
  let openOpen = $state(true);
  let openDone = $state(true);
  // `null` = fora da edição, `"new"` = criando aba, número = renomeando aquela aba.
  let editing = $state<number | "new" | null>(null);
  let draft = $state("");

  const RELEASES_URL = "https://github.com/ja1felipe/todo-taskbar/releases/latest";
  const UPDATE_INTERVAL = 6 * 60 * 60 * 1000;
  const SYNC_INTERVAL = 15 * 60 * 1000;

  type UpdateStatus = "idle" | "available" | "downloading" | "installing" | "error";

  let updateStatus = $state<UpdateStatus>("idle");
  let updateVersion = $state("");
  // -1 quando o servidor não informa o tamanho total do download.
  let updateProgress = $state(-1);
  let updateMessage = $state("");
  let checking = $state(false);
  let canSelfUpdate = $state(false);
  let pendingUpdate: Update | null = null;
  let updateTimer: ReturnType<typeof setInterval> | undefined;

  // Sincronização (Supabase). `auth` nulo = sem conta conectada.
  let auth = $state<AuthInfo | null>(null);
  let showLogin = $state(false);
  let loginEmail = $state("");
  let loginPassword = $state("");
  let loginError = $state("");
  let signingIn = $state(false);
  let syncing = $state(false);
  let syncNote = $state("");
  let syncError = $state(false);
  let syncTimer: ReturnType<typeof setInterval> | undefined;
  let unlistenFocus: (() => void) | undefined;
  // Canal de Realtime e client do Supabase usados só para receber avisos de
  // mudança; quem fala com o banco local continua sendo o Rust.
  let supa: SupabaseClient | null = null;
  let channel: RealtimeChannel | null = null;
  let realtimeUserId: string | null = null;
  // Token com que o canal atual foi autorizado. O RLS do Realtime é avaliado no
  // subscribe, então ao renovar o token é preciso reconstruir o canal.
  let realtimeToken: string | null = null;
  // Marca que o canal caiu; força a reconstrução no próximo `setupRealtime`.
  let realtimeBroken = false;
  let syncQueued = false;
  let syncDebounce: ReturnType<typeof setTimeout> | undefined;
  // Offline: o que for alterado continua indo pro banco local (com `dirty`) e
  // sobe quando a conexão voltar.
  let online = $state(typeof navigator === "undefined" ? true : navigator.onLine);
  let pendingChanges = $state(0);
  let retryTimer: ReturnType<typeof setTimeout> | undefined;
  let retryDelay = 0;

  let active = $derived(tabs.find((t) => t.id === activeId) ?? tabs[0]);
  let pending = $derived(active?.todos.filter((t) => !t.done) ?? []);
  let finished = $derived(active?.todos.filter((t) => t.done) ?? []);
  let remaining = $derived(pending.length);
  let isEditing = $derived(editing !== null);
  let canCreate = $derived(tabs.length < MAX_TABS);

  // Texto do botão de sync no rodapé.
  let syncLabel = $derived.by(() => {
    if (syncing) return "sincronizando…";
    if (!online) return pendingChanges > 0 ? `offline · ${pendingChanges}` : "offline";
    if (syncNote) return syncNote;
    if (pendingChanges > 0) return `${pendingChanges} p/ subir`;
    return "sincronizar";
  });

  onMount(async () => {
    // O WebView do Android desenha atrás das barras de status e navegação
    // (edge-to-edge). Marcamos o <html> para colorir essa faixa; o CSS então
    // desloca o app para fora das barras usando `env(safe-area-inset-*)`.
    if (/Android/i.test(navigator.userAgent)) {
      document.documentElement.classList.add("android");
    }

    const fromDb = await invoke<Store>("load_state").catch(() => null);

    let imported: Store | null = null;
    if (fromDb?.tabs?.length) {
      tabs = fromDb.tabs;
      activeId = fromDb.activeId;
    } else {
      // Primeira execução com o banco: o que houver no `localStorage` vira a
      // carga inicial, para ninguém perder o que já estava cadastrado.
      imported = importFromLocalStorage();
      if (imported) {
        tabs = imported.tabs;
        activeId = imported.activeId;
      }
    }

    ready = true;
    // Só grava na primeira execução se veio algo do `localStorage`. Um banco
    // vazio fica vazio: a aba "Tarefas" padrão existe só na tela até o usuário
    // criar algo (ou o sync trazer dados), senão ela viraria uma aba órfã
    // duplicada no primeiro sync com outro dispositivo.
    if (imported) void persist();

    await restoreWidth();
    await trackWidth();

    canSelfUpdate = await invoke<boolean>("can_self_update").catch(() => false);
    void checkForUpdates();
    updateTimer = setInterval(() => void checkForUpdates(), UPDATE_INTERVAL);

    // Sem conta conectada nada acontece; `syncNow` já ignora esse caso.
    await refreshAuth();
    void refreshPending();
    void setupRealtime();
    void syncNow();

    unlistenFocus = await appWindow.onFocusChanged(({ payload: focused }) => {
      if (focused) void syncNow();
    });
    window.addEventListener("online", handleOnline);
    window.addEventListener("offline", handleOffline);
    document.addEventListener("visibilitychange", handleVisibility);
    syncTimer = setInterval(() => void syncNow(), SYNC_INTERVAL);
  });

  onDestroy(() => {
    if (updateTimer) clearInterval(updateTimer);
    if (syncTimer) clearInterval(syncTimer);
    if (syncDebounce) clearTimeout(syncDebounce);
    clearRetry();
    window.removeEventListener("online", handleOnline);
    window.removeEventListener("offline", handleOffline);
    document.removeEventListener("visibilitychange", handleVisibility);
    unlistenFocus?.();
    void teardownRealtime();
    void pendingUpdate?.close();
  });

  /**
   * Recupera a largura salva e reaplica. O valor guardado é em pixels lógicos
   * (CSS), não físicos, senão a janela apareceria com outro tamanho em um monitor
   * com escala diferente.
   */
  async function restoreWidth() {
    const saved = Number(localStorage.getItem(WIDTH_KEY));
    if (!Number.isFinite(saved) || saved <= 0) return;

    const scale = await appWindow.scaleFactor();
    void invoke("set_width", { width: Math.round(saved * scale) }).catch(() => {});
  }

  /** Salva a largura a cada redimensionamento, para a próxima abertura manter. */
  async function trackWidth() {
    await appWindow.onResized(async () => {
      const [{ width }, scale] = await Promise.all([
        appWindow.innerSize(),
        appWindow.scaleFactor(),
      ]);
      const logical = Math.round(width / scale);
      if (logical > 0) {
        localStorage.setItem(WIDTH_KEY, String(Math.min(logical, MAX_STORED_WIDTH)));
      }
    });
  }

  /**
   * Lê o `localStorage` das versões anteriores do app. Só é chamado quando o
   * banco ainda está vazio; depois disso o banco é a fonte da verdade.
   */
  function importFromLocalStorage(): Store | null {
    const legacy = readJson<Todo[]>("todos");
    const stored = readJson<Partial<Store>>("todobar.tabs");

    let parsed: Tab[] | null = null;
    if (stored && Array.isArray(stored.tabs) && stored.tabs.length > 0) {
      parsed = stored.tabs.slice(0, MAX_TABS).map(sanitizeTab);
    } else if (Array.isArray(legacy)) {
      parsed = [
        { ...DEFAULT_TAB, todos: legacy.filter(comId).map(sanitizeTodo) },
      ];
    }

    // As chaves saem daqui para o mesmo dado não ser importado duas vezes.
    localStorage.removeItem("todos");
    localStorage.removeItem("todobar.tabs");
    if (!parsed) return null;

    return {
      tabs: parsed,
      activeId: parsed.some((t) => t.id === stored?.activeId)
        ? (stored?.activeId as number)
        : parsed[0].id,
    };
  }

  function readJson<T>(key: string): T | null {
    try {
      const raw = localStorage.getItem(key);
      return raw ? (JSON.parse(raw) as T) : null;
    } catch {
      return null;
    }
  }

  function sanitizeTodo(todo: Todo): Todo {
    return {
      id: todo.id,
      text: (todo.text ?? "").toString(),
      done: Boolean(todo.done),
      // O `localStorage` antigo não guardava horário, então a conclusão é
      // reanotada pelo backend na próxima vez que o item for concluído.
      completedAt: null,
    };
  }

  function sanitizeTab(tab: Tab): Tab {
    return {
      id: tab.id,
      name: (tab.name ?? "").slice(0, MAX_NAME) || DEFAULT_TAB.name,
      todos: Array.isArray(tab.todos) ? tab.todos.filter(comId).map(sanitizeTodo) : [],
    };
  }

  /** Sem `id` o item não tem como existir no banco, então é descartado. */
  function comId(todo: Partial<Todo>): todo is Todo {
    return typeof todo?.id === "number";
  }

  /**
   * Grava o estado e adota o que voltou do banco. As escritas são enfileiradas
   * porque cada `save_state` reenvia o estado inteiro: duas chamadas
   * simultâneas poderiam responder fora de ordem e travar uma a outra num
   * estado velho.
   */
  let fila: Promise<void> = Promise.resolve();
  function persist(): Promise<void> {
    if (!ready) return Promise.resolve();

    fila = fila
      .then(async () => {
        const salvo = await invoke<Store>("save_state", { store: { tabs, activeId } });
        // O backend pode ter preenchido um `completedAt` que ainda não tínhamos.
        tabs = salvo.tabs;
        activeId = salvo.activeId;
        void refreshPending();
        // Mudança local: sobe logo (com debounce) para o outro dispositivo
        // receber pelo Realtime em vez de esperar o próximo ciclo.
        scheduleSync();
      })
      .catch((erro) => {
        console.error("não foi possível salvar no banco", erro);
      });

    return fila;
  }

  function nextId() {
    return tabs.reduce((max, tab) => Math.max(max, tab.id), 0) + 1;
  }

  /**
   * O ID de TODO é chave primária da tabela, então precisa ser único entre
   * todas as abas — não serve um contador por aba. `Date.now()` resolve, e o
   * laço cobre dois itens criados no mesmo milissegundo.
   */
  function nextTodoId() {
    const usados = new Set(tabs.flatMap((tab) => tab.todos.map((t) => t.id)));
    let id = Date.now();
    while (usados.has(id)) id += 1;
    return id;
  }

  function selectTab(id: number) {
    if (id === activeId) return;
    if (isEditing) commitEdit();
    activeId = id;
    newTodo = "";
    void persist();
  }

  function startCreate() {
    if (!canCreate) return;
    editing = "new";
    draft = "";
  }

  function startRename(id: number) {
    editing = id;
    draft = tabs.find((t) => t.id === id)?.name ?? "";
  }

  function commitEdit() {
    if (editing === null) return;
    const target = editing;
    const name = draft.trim().slice(0, MAX_NAME);
    editing = null;
    draft = "";

    if (target === "new") {
      if (!canCreate) return;
      const tab: Tab = {
        id: nextId(),
        name: name || `Aba ${tabs.length + 1}`,
        todos: [],
      };
      tabs = [...tabs, tab];
      activeId = tab.id;
    } else if (name) {
      tabs = tabs.map((t) => (t.id === target ? { ...t, name } : t));
    }

    void persist();
  }

  function cancelEdit() {
    editing = null;
    draft = "";
  }

  function removeTab(id: number) {
    // A última aba é preservada para o conteúdo nunca ficar sem aba ativa.
    if (tabs.length <= 1) return;
    const index = tabs.findIndex((t) => t.id === id);
    tabs = tabs.filter((t) => t.id !== id);
    if (activeId === id) {
      activeId = tabs[Math.max(0, index - 1)]?.id ?? tabs[0].id;
    }
    if (editing === id) cancelEdit();
    void persist();
  }

  /** Aplica a alteração só nos TODOs da aba ativa. */
  function mapActiveTodos(fn: (todos: Todo[]) => Todo[]) {
    tabs = tabs.map((t) => (t.id === activeId ? { ...t, todos: fn(t.todos) } : t));
    void persist();
  }

  function addTodo(event: SubmitEvent) {
    event.preventDefault();
    const text = newTodo.trim();
    if (!text) return;
    mapActiveTodos((todos) => [
      ...todos,
      { id: nextTodoId(), text, done: false, completedAt: null },
    ]);
    newTodo = "";
  }

  function toggleTodo(id: number) {
    mapActiveTodos((todos) => todos.map((t) => (t.id === id ? { ...t, done: !t.done } : t)));
  }

  function removeTodo(id: number) {
    mapActiveTodos((todos) => todos.filter((t) => t.id !== id));
  }

  function clearDone() {
    mapActiveTodos((todos) => todos.filter((t) => !t.done));
  }

  /** Desmarca todas as concluídas, devolvendo-as para a lista em aberto. */
  function uncheckAllDone() {
    mapActiveTodos((todos) => todos.map((t) => (t.done ? { ...t, done: false } : t)));
  }

  function autofocus(node: HTMLInputElement) {
    node.focus();
    node.select();
  }

  /**
   * Redimensiona arrastando a borda esquerda. A janela não tem decorações, então
   * o GTK não oferece nenhuma alça nativa; e ancorar pela esquerda é o que mantém
   * a direita presa no ícone da bandeja enquanto a largura muda.
   */
  async function startResize(event: PointerEvent) {
    const grip = event.currentTarget as HTMLElement;
    // A captura pode falhar (ex.: pointerId sem botão pressionado) e abortaria
    // o handler inteiro, então o resize é ignorado sem erro.
    try {
      grip.setPointerCapture(event.pointerId);
    } catch {
      /* segue sem captura */
    }

    const startX = event.clientX;
    const [{ width, height }, scale] = await Promise.all([
      appWindow.innerSize(),
      appWindow.scaleFactor(),
    ]);

    const onMove = (move: PointerEvent) => {
      const next = width + Math.round((move.clientX - startX) * scale);
      void invoke("set_width", { width: Math.max(MIN_WIDTH, next) }).catch(() => {});
    };

    const stop = (up: PointerEvent) => {
      try {
        grip.releasePointerCapture(up.pointerId);
      } catch {
        /* já liberado */
      }
      grip.removeEventListener("pointermove", onMove);
      grip.removeEventListener("pointerup", stop);
      grip.removeEventListener("pointercancel", stop);
    };

    grip.addEventListener("pointermove", onMove);
    grip.addEventListener("pointerup", stop);
    grip.addEventListener("pointercancel", stop);
  }

  /**
   * Consulta o endpoint configurado no updater. Erros de rede são silenciosos:
   * estar offline não deve virar alerta na tela.
   */
  async function checkForUpdates() {
    if (checking || updateStatus === "downloading" || updateStatus === "installing") return;
    checking = true;
    try {
      const update = await check();
      if (update) {
        await pendingUpdate?.close();
        pendingUpdate = update;
        updateVersion = update.version;
        updateStatus = "available";
      } else if (updateStatus === "available") {
        updateStatus = "idle";
      }
    } catch (error) {
      console.error("não foi possível verificar atualizações", error);
    } finally {
      checking = false;
    }
  }

  /** Baixa e instala a atualização pendente, mostrando o progresso. */
  async function installUpdate() {
    const update = pendingUpdate;
    if (!update) return;

    updateStatus = "downloading";
    updateProgress = -1;
    let total = 0;
    let received = 0;

    try {
      await update.downloadAndInstall((event) => {
        if (event.event === "Started") {
          total = event.data.contentLength ?? 0;
          updateProgress = total > 0 ? 0 : -1;
        } else if (event.event === "Progress") {
          received += event.data.chunkLength;
          if (total > 0) updateProgress = received / total;
        } else if (event.event === "Finished") {
          updateStatus = "installing";
        }
      });

      // No Windows o instalador assume e encerra o app sozinho; no Linux e no
      // macOS é preciso relançar para subir na versão nova.
      await relaunch();
    } catch (error) {
      console.error("falha ao atualizar", error);
      updateMessage = "Não foi possível atualizar automaticamente nesta instalação.";
      updateStatus = "error";
    }
  }

  function openReleases() {
    void openUrl(RELEASES_URL);
  }

  function dismissUpdate() {
    updateStatus = "idle";
  }

  async function refreshAuth() {
    auth = await invoke<AuthInfo | null>("auth_status").catch(() => null);
  }

  /**
   * Espera a fila de gravações locais antes de sincronizar: o merge lê o banco e
   * não pode rodar no meio de um `save_state` ainda em voo.
   */
  async function syncNow() {
    if (!auth) return;
    // Sem conexão não adianta tentar: as alterações já estão salvas localmente
    // (com `dirty`) e sobem quando o "online" disparar.
    if (!online) return;
    // Se um sync já está rodando, marca para repetir ao terminar: um aviso de
    // Realtime que chega no meio de um ciclo não pode ser perdido.
    if (syncing) {
      syncQueued = true;
      return;
    }
    syncing = true;
    syncError = false;
    clearRetry();
    try {
      await fila;
      await invoke("sync_now");
      // O merge pode ter trazido mudanças, então a tela recarrega do banco.
      const fromDb = await invoke<Store>("load_state");
      if (fromDb?.tabs?.length) {
        tabs = fromDb.tabs;
        activeId = fromDb.activeId;
      }
      syncNote = "sincronizado";
      retryDelay = 0;
    } catch (error) {
      console.error("não foi possível sincronizar", error);
      syncError = true;
      syncNote = "sem conexão";
      scheduleRetry();
    } finally {
      syncing = false;
      // O Rust é quem renova o token; o canal precisa receber o mais recente.
      void setupRealtime();
      void refreshPending();
      setTimeout(() => {
        if (!syncError) syncNote = "";
      }, 2500);
    }
    if (syncQueued) {
      syncQueued = false;
      void syncNow();
    }
  }

  /** Quantas alterações locais ainda não subiram (abas + TODOs). */
  async function refreshPending() {
    pendingChanges = await invoke<number>("pending_count").catch(() => pendingChanges);
  }

  function handleOnline() {
    online = true;
    retryDelay = 0;
    clearRetry();
    // O canal provavelmente morreu enquanto esteve offline; reconstrói.
    realtimeBroken = true;
    void refreshPending();
    void setupRealtime();
    void syncNow();
  }

  function handleOffline() {
    online = false;
    clearRetry();
  }

  /**
   * No mobile o app fica suspenso em segundo plano e o WebSocket morre em
   * silêncio (sem evento de erro), então o que mudou nesse período não chega.
   * Ao voltar para a frente, reconstruímos o canal e sincronizamos.
   */
  function handleVisibility() {
    if (document.visibilityState !== "visible") return;
    realtimeBroken = true;
    void setupRealtime();
    void syncNow();
  }

  /**
   * Reagenda um sync quando a falha acontece com a rede ainda "online" (ex.:
   * servidor fora do ar). O intervalo dobra até 60s para não martelar.
   */
  function scheduleRetry() {
    if (retryTimer || !auth || !online) return;
    retryDelay = retryDelay ? Math.min(retryDelay * 2, 60000) : 5000;
    retryTimer = setTimeout(() => {
      retryTimer = undefined;
      void syncNow();
    }, retryDelay);
  }

  function clearRetry() {
    if (retryTimer) {
      clearTimeout(retryTimer);
      retryTimer = undefined;
    }
  }

  /**
   * Abre (ou atualiza) o canal de Realtime do usuário. Não é ele que grava
   * nada: cada evento apenas dispara um sync, que faz o pull+merge de verdade.
   * O `postgres_changes` respeita as políticas de RLS pelo token informado.
   */
  async function setupRealtime() {
    const cfg = await invoke<RealtimeConfig | null>("realtime_config").catch(() => null);
    if (!cfg) {
      await teardownRealtime();
      return;
    }

    if (!supa) {
      // O Supabase aqui só serve para o canal: nada de sessão nem refresh de
      // token no JS, para não competir com o refresh que o Rust já faz.
      //
      // `accessToken` é obrigatório: o `supabase-js` sempre injeta no Realtime
      // um callback que, sem sessão no JS, devolve a anon key e sobrescreve
      // qualquer `setAuth` a cada heartbeat. Sem passar o token do Rust por
      // aqui, o RLS avalia o canal como `anon` e os eventos passam a ser
      // descartados (o canal fica "mudo" alguns segundos após o subscribe).
      supa = createClient(cfg.url, cfg.anon, {
        auth: { persistSession: false, autoRefreshToken: false, detectSessionInUrl: false },
        accessToken: async () => {
          const atual = await invoke<RealtimeConfig | null>("realtime_config").catch(() => null);
          return atual?.accessToken ?? "";
        },
        realtime: {
          heartbeatCallback: (status) => {
            // Conexão caiu em silêncio: tenta reabrir já.
            if (status === "disconnected") void supa?.realtime.connect();
          },
        },
      });
    }

    // Reaproveita o canal só se for a mesma conta, com o mesmo token e sem
    // falha pendente. O RLS do Realtime é avaliado no subscribe, então token
    // renovado (ou conta trocada) exige reconstruir o canal.
    if (
      channel &&
      !realtimeBroken &&
      realtimeUserId === cfg.userId &&
      realtimeToken === cfg.accessToken
    ) {
      return;
    }

    if (channel) {
      await supa.removeChannel(channel);
      channel = null;
    }

    realtimeToken = cfg.accessToken;
    realtimeUserId = cfg.userId;
    realtimeBroken = false;

    const created = supa
      .channel(`todo-taskbar:${cfg.userId}`)
      .on("postgres_changes", { event: "*", schema: "public", table: "tabs" }, scheduleSync)
      .on("postgres_changes", { event: "*", schema: "public", table: "todos" }, scheduleSync);

    // O token precisa estar no payload do `phx_join`. O `supabase-js` chama
    // `realtime.setAuth()` uma vez dentro do `createClient`, antes deste canal
    // existir, então o `updateJoinPayload` nunca roda para ele; e o socket pode
    // enviar o join no open antes do callback async de `accessToken` resolver.
    // Zeramos o valor em cache e reaplicamos o token para o join já sair
    // autorizado (com RLS de `auth.uid()`), antes de assinar.
    supa.realtime.accessTokenValue = null;
    await supa.realtime.setAuth(cfg.accessToken);

    channel = created;
    created.subscribe((status) => {
      // Ignora callbacks de um canal que já foi substituído/descartado.
      if (channel !== created) return;
      if (status === "SUBSCRIBED") {
        // Conectou (ou reconectou): prova de que há rede, então sincroniza o
        // que ficou pendente e o que mudou do outro lado enquanto isso.
        online = true;
        void syncNow();
      } else if (status === "CHANNEL_ERROR" || status === "TIMED_OUT" || status === "CLOSED") {
        console.warn("canal de realtime indisponível", status);
        realtimeBroken = true;
      }
    });
  }

  async function teardownRealtime() {
    if (supa && channel) await supa.removeChannel(channel);
    channel = null;
    realtimeUserId = null;
    realtimeToken = null;
    realtimeBroken = false;
  }

  /** Vários eventos seguidos viram um único sync (ex.: salvar uma tarefa mexe
   *  na aba e no TODO, e o eco do nosso próprio push também chega). */
  function scheduleSync() {
    if (syncDebounce) clearTimeout(syncDebounce);
    syncDebounce = setTimeout(() => {
      syncDebounce = undefined;
      void syncNow();
    }, 400);
  }


  function openLogin() {
    loginError = "";
    showLogin = true;
  }

  function closeLogin() {
    showLogin = false;
    loginPassword = "";
    loginError = "";
  }

  async function submitLogin(event: SubmitEvent) {
    event.preventDefault();
    if (signingIn) return;
    signingIn = true;
    loginError = "";
    try {
      auth = await invoke<AuthInfo>("sign_in", {
        email: loginEmail.trim(),
        password: loginPassword,
      });
      closeLogin();
      await syncNow();
    } catch (error) {
      console.error("sign_in failed", error);
      loginError = typeof error === "string" ? error : JSON.stringify(error);
    } finally {
      signingIn = false;
    }
  }

  async function signOut() {
    await invoke("sign_out").catch(() => {});
    await teardownRealtime();
    auth = null;
    syncNote = "";
    syncError = false;
    clearRetry();
    // O `sign_out` apaga a cópia local (o servidor mantém). A tela volta ao
    // estado inicial para não mostrar/persistir dados da conta que saiu.
    tabs = [{ ...DEFAULT_TAB }];
    activeId = DEFAULT_TAB.id;
    pendingChanges = 0;
  }

  function hide() {
    appWindow.hide();
  }

  function onKeydown(event: KeyboardEvent) {
    if (event.key === "Escape") {
      event.preventDefault();
      // Durante a edição o Escape cancela em vez de fechar a janela.
      if (showLogin) closeLogin();
      else if (isEditing) cancelEdit();
      else hide();
      return;
    }

    if (event.key === "q" && (event.ctrlKey || event.metaKey)) {
      event.preventDefault();
      invoke("quit").catch(() => {});
    }
  }

  function onPointerDown(event: MouseEvent) {
    const target = event.target as HTMLElement | null;
    if (target?.closest?.("main")) return;
    hide();
  }
</script>

<svelte:window onkeydown={onKeydown} onpointerdown={onPointerDown} />

<main>
  <div
    class="resize-grip"
    role="separator"
    aria-orientation="vertical"
    aria-label="Redimensionar janela"
    title="Arraste para ajustar a largura"
    onpointerdown={startResize}
  ></div>

  <div class="content">
    {#if updateStatus !== "idle"}
      <div class="update" class:update-problem={updateStatus === "error"} role="status">
        {#if updateStatus === "available"}
          <span class="update-text">
            Nova versão {updateVersion} disponível{#if !canSelfUpdate} — baixe manualmente{/if}.
          </span>
          <button class="update-action" onclick={canSelfUpdate ? installUpdate : openReleases}>
            {canSelfUpdate ? "Atualizar" : "Baixar"}
          </button>
          <button class="update-close" aria-label="Dispensar" onclick={dismissUpdate}>
            &times;
          </button>
        {:else if updateStatus === "downloading"}
          <span class="update-text">
            Baixando atualização{updateProgress >= 0
              ? ` — ${Math.round(updateProgress * 100)}%`
              : "…"}
          </span>
        {:else if updateStatus === "installing"}
          <span class="update-text">Instalando… o app vai reiniciar.</span>
        {:else if updateStatus === "error"}
          <span class="update-text">{updateMessage}</span>
          <button class="update-action" onclick={openReleases}>Baixar</button>
          <button class="update-close" aria-label="Dispensar" onclick={dismissUpdate}>
            &times;
          </button>
        {/if}
      </div>
    {/if}

    <header>
      <h1>{active?.name ?? "Todo Taskbar"}</h1>
      <span class="count">{remaining} pendentes</span>
    </header>

    <form onsubmit={addTodo}>
      <input bind:value={newTodo} placeholder="Adicionar tarefa..." />
      <button type="submit" aria-label="Adicionar">+</button>
    </form>

    <div class="sections">
      <Section title="Em aberto" count={pending.length} bind:open={openOpen}>
        {#each pending as todo (todo.id)}
          <TodoItem {todo} ontoggle={toggleTodo} onremove={removeTodo} />
        {:else}
          <li class="empty">Nenhuma tarefa em aberto</li>
        {/each}
      </Section>

      <Section title="Finalizadas" count={finished.length} bind:open={openDone}>
        {#snippet actions()}
          <button class="clear" onclick={uncheckAllDone} disabled={finished.length === 0}>
            desmarcar tudo
          </button>
          <button class="clear" onclick={clearDone} disabled={finished.length === 0}>limpar</button>
        {/snippet}

        {#each finished as todo (todo.id)}
          <TodoItem {todo} ontoggle={toggleTodo} onremove={removeTodo} />
        {:else}
          <li class="empty">Nenhuma tarefa finalizada</li>
        {/each}
      </Section>
    </div>

    <footer>
      <span>{active?.todos.length ?? 0} no total</span>
      <span class="footer-links">
        {#if auth}
          <button
            class="check"
            class:sync-error={syncError}
            onclick={syncNow}
            disabled={syncing || !online}
            title={online
              ? (auth.email ?? auth.userId)
              : "Sem conexão: as alterações serão enviadas automaticamente"}
          >
            {syncLabel}
          </button>
          <button class="check" onclick={signOut}>desconectar</button>
        {:else}
          <button class="check" onclick={openLogin}>entrar</button>
        {/if}
        <button class="check" onclick={checkForUpdates} disabled={checking}>
          {checking ? "verificando…" : "verificar"}
        </button>
        <button class="quit" onclick={() => invoke("quit").catch(() => {})}>Sair</button>
      </span>
    </footer>
  </div>

  {#if showLogin}
    <form class="login" onsubmit={submitLogin}>
      <label for="login-email">Entrar para sincronizar</label>
      <input
        id="login-email"
        type="email"
        bind:value={loginEmail}
        placeholder="email"
        autocomplete="username"
      />
      <input
        type="password"
        bind:value={loginPassword}
        placeholder="senha"
        autocomplete="current-password"
      />
      {#if loginError}
        <small class="login-error">{loginError}</small>
      {/if}
      <div class="login-actions">
        <button type="button" onclick={closeLogin}>Cancelar</button>
        <button type="submit" disabled={signingIn || !loginEmail || !loginPassword}>
          {signingIn ? "Entrando…" : "Entrar"}
        </button>
      </div>
    </form>
  {/if}

  <nav class="tabs" aria-label="Abas">
    {#each tabs as tab (tab.id)}
      <div class="tab" class:active={tab.id === activeId}>
        <button
          class="tab-name"
          title="{tab.name} — duplo clique para renomear"
          onclick={() => selectTab(tab.id)}
          ondblclick={() => startRename(tab.id)}
        >
          <span class="tab-label">{tab.name}</span>
        </button>
        {#if tabs.length > 1}
          <button
            class="tab-remove"
            title="Remover aba"
            aria-label="Remover aba {tab.name}"
            onclick={() => removeTab(tab.id)}
          >
            &times;
          </button>
        {/if}
      </div>
    {/each}

    {#if isEditing}
      <button
        class="tab-add"
        title="Cancelar"
        aria-label="Cancelar edição"
        onclick={cancelEdit}
      >
        &times;
      </button>
    {:else}
      <button
        class="tab-add"
        title={canCreate ? "Criar aba" : `Máximo de ${MAX_TABS} abas`}
        aria-label="Criar aba"
        disabled={!canCreate}
        onclick={startCreate}
      >
        +
      </button>
    {/if}
  </nav>

  <!-- Fora de `nav.tabs`: o campo precisa de largura horizontal e não pode
       herdar o `overflow: auto` nem ser cortado pela faixa estreita. -->
  {#if isEditing}
    <form class="tab-edit" onsubmit={(e) => { e.preventDefault(); commitEdit(); }}>
      <label for="tab-name">
        {editing === "new" ? "Nome da nova aba" : "Novo nome da aba"}
      </label>
      <input
        id="tab-name"
        use:autofocus
        bind:value={draft}
        maxlength={MAX_NAME}
        placeholder={editing === "new" ? "Ex.: Trabalho" : "Ex.: Pessoal"}
        onblur={commitEdit}
      />
      <small>Enter confirma &middot; Esc cancela &middot; {draft.length}/{MAX_NAME}</small>
    </form>
  {/if}
</main>

<style>
  :global(:root) {
    color-scheme: dark;
  }

  :global(html),
  :global(body) {
    margin: 0;
    padding: 0;
    width: 100%;
    height: 100%;
    background: transparent;
    overflow: hidden;
    user-select: none;
  }

  :global(body) {
    font-family: system-ui, -apple-system, "Segoe UI", Roboto, Ubuntu, sans-serif;
  }

  /* No Android o app fica edge-to-edge; esta cor preenche a faixa atrás das
     barras do sistema. No desktop o fundo continua transparente. */
  :global(html.android),
  :global(html.android body) {
    background: #1c1c1e;
  }

  main {
    /* `inset` em vez de `margin` + `calc(100vh - 12px)`: a conta com vh fecha
       exatamente no limite da viewport, então qualquer arredondamento sub-pixel
       transborda 1px e cria uma barra de rolagem fantasma no documento.
       Com `position: fixed` o navegador é quem dimensiona a caixa, e um
       elemento fixo não contribui para a área rolável do documento. */
    --pad: 0.75rem;
    --edge: calc(-1 * var(--pad) - 1px);
    position: fixed;
    inset: 6px;
    /* No Android (edge-to-edge) recuamos o app para fora da barra de status e
       dos botões de navegação; no desktop as safe-areas são 0 e sobram os 6px.
       A segunda linha cai no primeiro valor se `env()` não for suportado. */
    inset: max(6px, env(safe-area-inset-top)) max(6px, env(safe-area-inset-right))
      max(6px, env(safe-area-inset-bottom)) max(6px, env(safe-area-inset-left));
    box-sizing: border-box;
    display: flex;
    flex-direction: row;
    padding: var(--pad);
    border-radius: 0.6rem;
    background: #1c1c1e;
    color: #e8e8ea;
    border: 1px solid #2f2f33;
    box-shadow: 0 8px 24px rgb(0 0 0 / 45%);
  }

  /* Alça de redimensionamento na borda esquerda: a direita fica ancorada no
     ícone da bandeja, então é por aqui que a largura cresce. */
  .resize-grip {
    position: absolute;
    left: 0;
    top: 0;
    bottom: 0;
    width: 9px;
    z-index: 1;
    cursor: ew-resize;
  }

  .resize-grip::before {
    content: "";
    position: absolute;
    left: 3px;
    top: 50%;
    width: 2px;
    height: 2.4rem;
    transform: translateY(-50%);
    border-radius: 2px;
    background: #3d3d45;
    transition: background 0.12s ease;
  }

  .resize-grip:hover::before {
    background: #4a8cff;
  }

  .content {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 0.6rem;
    /* As abas usam margem negativa para encostar na borda direita, o que
       anula o `padding` de `main` desse lado. Sem esta folga o conteúdo
       ficaria colado nelas; com ela, o respiro fica igual ao da esquerda. */
    padding-right: var(--pad);
  }

  /* Faixa de atualização disponível, no topo do conteúdo. */
  .update {
    display: flex;
    align-items: center;
    gap: 0.45rem;
    padding: 0.42rem 0.5rem;
    border-radius: 0.4rem;
    background: #1f3350;
    border: 1px solid #2f5a8f;
    color: #d3e3ff;
    font-size: 0.78rem;
  }

  .update-problem {
    background: #3a2626;
    border-color: #6b3a3a;
    color: #ffd9d9;
  }

  .update-text {
    flex: 1;
    min-width: 0;
    line-height: 1.3;
  }

  .update-action {
    flex-shrink: 0;
    padding: 0.22rem 0.5rem;
    border: none;
    border-radius: 0.3rem;
    background: #4a8cff;
    color: #fff;
    font-size: 0.75rem;
    font-weight: 600;
    cursor: pointer;
  }

  .update-action:hover {
    background: #5f9bff;
  }

  .update-close {
    flex-shrink: 0;
    padding: 0 0.1rem;
    border: none;
    background: transparent;
    color: inherit;
    font-size: 0.95rem;
    line-height: 1;
    opacity: 0.7;
    cursor: pointer;
  }

  .update-close:hover {
    opacity: 1;
  }

  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.4rem;
    padding: 0 0.15rem;
  }

  h1 {
    margin: 0;
    font-size: 1rem;
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .count {
    flex-shrink: 0;
    font-size: 0.8rem;
    color: #9a9aa0;
  }

  form {
    display: flex;
    gap: 0.4rem;
  }

  input:not([type]) {
    flex: 1;
    min-width: 0;
    padding: 0.45rem 0.55rem;
    font-size: 0.9rem;
    color: inherit;
    border-radius: 0.4rem;
    border: 1px solid #34343a;
    background: #141416;
    outline: none;
    user-select: text;
  }

  input:focus {
    border-color: #4a8cff;
  }

  form button {
    width: 2rem;
    font-size: 1.2rem;
    line-height: 1;
    border-radius: 0.4rem;
    border: 1px solid #34343a;
    background: #26262b;
    color: #e8e8ea;
    cursor: pointer;
  }

  form button:hover {
    background: #303036;
  }

  .sections {
    display: flex;
    flex-direction: column;
    gap: 0.6rem;
    flex: 1;
    min-height: 0;
    overflow-y: auto;
  }

  .empty {
    justify-content: center;
    background: transparent;
    color: #6e6e76;
    font-size: 0.9rem;
    padding: 0.7rem 0;
  }

  footer {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 0 0.15rem 0.15rem;
    font-size: 0.8rem;
    color: #6e6e76;
  }

  .clear {
    border: none;
    background: transparent;
    color: #6e6e76;
    font-size: 0.7rem;
    cursor: pointer;
    padding: 0.1rem 0.3rem;
    border-radius: 0.3rem;
  }

  .clear:hover:not(:disabled) {
    color: #e8e8ea;
  }

  .clear:disabled {
    opacity: 0.35;
    cursor: default;
  }

  .footer-links {
    display: flex;
    align-items: center;
    justify-content: flex-end;
    flex-wrap: wrap;
    gap: 0.2rem 0.55rem;
  }

  .check.sync-error {
    color: #ff9d9d;
  }

  .check {
    border: none;
    background: transparent;
    color: #6e6e76;
    font-size: 0.72rem;
    cursor: pointer;
    padding: 0;
  }

  .check:hover:not(:disabled) {
    color: #e8e8ea;
  }

  .check:disabled {
    opacity: 0.6;
    cursor: default;
  }

  .quit {
    border: none;
    background: transparent;
    color: #6e6e76;
    font-size: 0.72rem;
    cursor: pointer;
    padding: 0;
  }

  .quit:hover {
    color: #ff6b6b;
  }

  /* Abas: coladas na lateral direita, ocupando toda a altura do container. */
  .tabs {
    position: relative;
    flex: 0 0 auto;
    width: 3.5rem;
    margin: var(--edge) var(--edge) var(--edge) 0;
    /* O `padding` à direita precisa acomodar o quanto o botão de remover
       transborda da aba (~42% da própria largura), senão o `overflow: auto`
       da faixa corta o círculo. */
    padding: 0.5rem 0.85rem 0.5rem 0.45rem;
    box-sizing: border-box;
    display: flex;
    flex-direction: column;
    align-items: stretch;
    gap: 3px;
    overflow-y: auto;
    background: #171719;
    border-left: 1px solid #2f2f33;
    border-radius: 0 0.5rem 0.5rem 0;
  }

  .tab {
    position: relative;
    border-radius: 0.3rem;
  }

  .tab-name {
    display: block;
    width: 100%;
    padding: 0.55rem 0.3rem;
    border: none;
    border-radius: 0.35rem;
    background: transparent;
    color: #a0a0a8;
    font: inherit;
    font-size: 0.88rem;
    font-weight: 500;
    cursor: pointer;
  }

  /* O texto vertical precisa morar num elemento normal: o WebKit descarta
     `writing-mode` em controles de formulário (<button>), então aplicar isso no
     próprio botão deixa o rótulo horizontal e cortado. */
  .tab-label {
    display: block;
    width: fit-content;
    margin: 0 auto;
    writing-mode: vertical-rl;
    text-orientation: mixed;
    max-height: 11rem;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .tab-name:hover {
    background: #222227;
    color: #cfcfd4;
  }

  .tab.active .tab-name {
    background: #2a2a30;
    color: #e8e8ea;
    box-shadow: inset -2px 0 0 #4a8cff;
  }

  /* Deslocado para fora do canto superior direito, com `translate` em ambas as
     direções: `right` negativo sozinho jogaria o botão para DENTRO da aba
     (a referência é a borda direita do pai). O `translate` também preserva o
     texto vertical começando no topo. */
  .tab-remove {
    position: absolute;
    top: 0;
    right: 0;
    z-index: 1;
    transform: translate(42%, -62%);
    writing-mode: horizontal-tb;
    width: 1.15rem;
    height: 1.15rem;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 0;
    border: none;
    border-radius: 999px;
    background: #35353c;
    color: #cfcfd4;
    font-size: 0.78rem;
    line-height: 1;
    cursor: pointer;
    opacity: 0;
  }

  .tab:hover .tab-remove,
  .tab.active .tab-remove {
    opacity: 1;
  }

  .tab-remove:hover {
    background: #ff6b6b;
    color: #fff;
  }

  .tab-add {
    margin-top: auto;
    padding: 0.4rem 0;
    border: none;
    border-radius: 0.35rem;
    background: transparent;
    color: #a0a0a8;
    font-size: 1.15rem;
    line-height: 1;
    cursor: pointer;
  }

  .tab-add:hover:not(:disabled) {
    background: #222227;
    color: #e8e8ea;
  }

  .tab-add:disabled {
    opacity: 0.3;
    cursor: default;
  }

  /* Popover do nome da aba, sobreposto ao container via z-index. */
  .tab-edit {
    position: absolute;
    right: 0.5rem;
    bottom: 0.5rem;
    z-index: 2;
    width: 13rem;
    display: flex;
    flex-direction: column;
    gap: 0.35rem;
    padding: 0.7rem;
    border-radius: 0.5rem;
    background: #202024;
    border: 1px solid #3a3a42;
    box-shadow: 0 6px 18px rgb(0 0 0 / 55%);
  }

  .tab-edit label {
    font-size: 0.75rem;
    color: #9a9aa0;
  }

  .tab-edit input {
    width: 100%;
    box-sizing: border-box;
    padding: 0.45rem 0.5rem;
    font-size: 0.9rem;
    color: inherit;
    border-radius: 0.4rem;
    border: 1px solid #4a8cff;
    background: #141416;
    outline: none;
    user-select: text;
  }

  .tab-edit small {
    font-size: 0.68rem;
    color: #6e6e76;
  }

  /* Popover de login, centralizado sobre o conteúdo. */
  .login {
    position: absolute;
    left: 50%;
    bottom: 3.6rem;
    z-index: 3;
    transform: translateX(-50%);
    width: min(14rem, calc(100% - 5.5rem));
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
    padding: 0.75rem;
    border-radius: 0.5rem;
    background: #202024;
    border: 1px solid #3a3a42;
    box-shadow: 0 6px 18px rgb(0 0 0 / 55%);
  }

  .login label {
    font-size: 0.75rem;
    color: #9a9aa0;
  }

  .login input {
    width: 100%;
    box-sizing: border-box;
    padding: 0.45rem 0.5rem;
    font-size: 0.85rem;
    color: inherit;
    border-radius: 0.4rem;
    border: 1px solid #34343a;
    background: #141416;
    outline: none;
    user-select: text;
  }

  .login input:focus {
    border-color: #4a8cff;
  }

  .login-error {
    color: #ff9d9d;
    font-size: 0.7rem;
    word-break: break-word;
    user-select: text;
  }

  .login-actions {
    display: flex;
    justify-content: flex-end;
    gap: 0.5rem;
  }

  .login-actions button {
    width: auto;
    padding: 0.35rem 0.7rem;
    border: none;
    border-radius: 0.3rem;
    font-size: 0.8rem;
    line-height: 1.2;
    white-space: nowrap;
    cursor: pointer;
  }

  .login-actions button[type="button"] {
    background: transparent;
    color: #9a9aa0;
  }

  .login-actions button[type="submit"] {
    background: #4a8cff;
    color: #fff;
    font-weight: 600;
  }

  .login-actions button:disabled {
    opacity: 0.5;
    cursor: default;
  }
</style>

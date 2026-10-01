<script lang="ts">
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { invoke } from "@tauri-apps/api/core";
  import { onMount } from "svelte";
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

  let active = $derived(tabs.find((t) => t.id === activeId) ?? tabs[0]);
  let pending = $derived(active?.todos.filter((t) => !t.done) ?? []);
  let finished = $derived(active?.todos.filter((t) => t.done) ?? []);
  let remaining = $derived(pending.length);
  let isEditing = $derived(editing !== null);
  let canCreate = $derived(tabs.length < MAX_TABS);

  onMount(async () => {
    const fromDb = await invoke<Store>("load_state").catch(() => null);

    if (fromDb?.tabs?.length) {
      tabs = fromDb.tabs;
      activeId = fromDb.activeId;
    } else {
      // Primeira execução com o banco: o que houver no `localStorage` vira a
      // carga inicial, para ninguém perder o que já estava cadastrado.
      const imported = importFromLocalStorage();
      if (imported) {
        tabs = imported.tabs;
        activeId = imported.activeId;
      }
    }

    ready = true;
    void persist();

    await restoreWidth();
    await trackWidth();
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
    void invoke("set_width", { width: Math.round(saved * scale) });
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
      void invoke("set_width", { width: Math.max(MIN_WIDTH, next) });
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

  function hide() {
    appWindow.hide();
  }

  function onKeydown(event: KeyboardEvent) {
    if (event.key === "Escape") {
      event.preventDefault();
      // Durante a edição o Escape cancela em vez de fechar a janela.
      if (isEditing) cancelEdit();
      else hide();
      return;
    }

    if (event.key === "q" && (event.ctrlKey || event.metaKey)) {
      event.preventDefault();
      invoke("quit");
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
    <header>
      <h1>{active?.name ?? "todobar"}</h1>
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
      <button class="quit" onclick={() => invoke("quit")}>Sair</button>
    </footer>
  </div>

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
    padding: 0 0.15rem;
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
</style>
